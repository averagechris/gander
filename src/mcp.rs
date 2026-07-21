//! `gander mcp`: the agent-facing MCP tool surface (docs/decisions.md D5).
//!
//! An MCP stdio server implemented with the official Rust SDK (`rmcp`) as a
//! thin adapter over the same [`crate::acp::AcpHandler`] dispatch that backs
//! `gander acp`. Spawned in a workspace, each tool call routes to that
//! workspace's live TUI instance through the instance registry
//! (docs/decisions.md D3), so harnesses see current viewed state, comments,
//! and focus; without a running TUI it falls back to a snapshot session
//! loaded at startup. Harnesses discover the typed tools natively — no wire
//! protocol explained in a prompt.

use std::{path::PathBuf, sync::mpsc};

use color_eyre::eyre::{Context as _, Result};
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    acp::AcpHandler,
    app::ReviewSession,
    config::InitialCommentState,
    diff::FileDiff,
    generated::GeneratedPolicy,
    jj::{JjBackend, ReviewTarget as JjReviewTarget},
    registry, review,
    state::{
        ActionIntent, Channel, CommentKind, CommentState, Identity, ReviewDisposition, ReviewState,
        ReviewTarget as StateReviewTarget, Salience, StepArtifact, StepArtifactKind,
        StepImportance, StepKind, WalkthroughStep,
    },
};

/// MCP server state: registry routing plus an in-process snapshot fallback.
///
/// [`ReviewSession`] is deliberately single-threaded (interior caches), so
/// the snapshot lives on its own dispatch thread and tool calls reach it
/// through a channel — the same shape as the TUI's live socket bridge.
pub struct GanderMcp {
    registry_dir: PathBuf,
    workspace_root: PathBuf,
    state_path: PathBuf,
    target: review::SessionTargetSpec,
    diff_files: Vec<String>,
    attention_files: Vec<FileDiff>,
    generated_policy: GeneratedPolicy,
    ignore_globs: Vec<String>,
    initial_comment_state: CommentState,
    agent_identity: Identity,
    snapshot: mpsc::Sender<SnapshotRequest>,
    tool_router: ToolRouter<Self>,
}

pub struct GanderMcpParams {
    pub overlay_path: PathBuf,
    pub state_path: PathBuf,
    pub registry_dir: PathBuf,
    pub workspace_root: PathBuf,
    pub target: JjReviewTarget,
    pub diff_files: Vec<String>,
    pub attention_files: Vec<FileDiff>,
    pub generated_policy: GeneratedPolicy,
    pub ignore_globs: Vec<String>,
    pub initial_comment_state: CommentState,
    pub agent_identity: Identity,
}

#[derive(Debug, Clone, Deserialize)]
struct SelectedReviewContext {
    repo: PathBuf,
    base: String,
    revision: String,
    files: Vec<FileDiff>,
}

struct SnapshotRequest {
    line: String,
    reply: mpsc::Sender<Option<Value>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FileDiffParams {
    /// Repository-relative file path as listed by `review_files`.
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PresentGotoParams {
    pub index: Option<usize>,
    pub step_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PresentFocusParams {
    pub path: String,
    pub line: u32,
    pub end_line: Option<u32>,
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetOrderingParams {
    /// File paths in suggested review order, highest priority first.
    pub paths: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FlagSectionParams {
    pub path: String,
    /// New-side line number the flag points at.
    pub line: Option<u64>,
    /// Why this section deserves extra scrutiny.
    pub reason: String,
    /// critical | high | medium | low (default high).
    pub priority: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DraftCommentParams {
    pub path: String,
    /// New-side line number the comment anchors to.
    pub line: Option<u64>,
    pub body: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ChangeDiffParams {
    /// A jj change id as listed by `stack_changes`.
    pub change_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReviewsCreateParams {
    /// Optional title. Equivalent to `gander reviews create --title <title>`.
    pub title: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IdParams {
    /// Full id or unambiguous id prefix.
    pub id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CommentAddParams {
    /// Diff file path. Mutually exclusive with `general`.
    pub path: Option<String>,
    /// Create a session-level comment with no file anchor.
    pub general: Option<bool>,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    pub body: String,
    pub kind: Option<CommentKind>,
    pub action: Option<ActionIntent>,
    /// Initial durable state. Only draft or todo are accepted.
    pub state: Option<InitialCommentState>,
    /// Annotation channel, like CLI `--channel`: onboarding, delegation,
    /// collaboration, or note. A todo in a channel that does not permit
    /// todos (onboarding/note) is stored as a draft instead. Omitted:
    /// state-derived default (todo -> delegation, else note).
    pub channel: Option<Channel>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CommentsParams {
    /// Optional annotation channel filter: onboarding, delegation, collaboration, or note.
    pub channel: Option<Channel>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CommentsReadyParams {
    /// Full ids or unambiguous prefixes. Mutually exclusive with all_drafts.
    pub ids: Option<Vec<String>>,
    /// Ready every active-session draft.
    pub all_drafts: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CommentSetStateParams {
    /// Full id or unambiguous id prefix. Equivalent to `gander comments set-state <id> <state>`.
    pub id: String,
    pub state: CommentState,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CommentReplyParams {
    /// Full id or unambiguous id prefix. Equivalent to `gander comments reply <id>`.
    pub id: String,
    /// Reply body text. Must contain non-whitespace text.
    pub body: String,
    /// Also mark the parent comment resolved after appending the reply.
    pub resolve: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CommentResolveParams {
    /// Full id or unambiguous id prefix. Equivalent to `gander comments resolve <id>`.
    pub id: String,
    /// Optional reply body to append before resolving.
    pub reply: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReviewDispositionParams {
    pub disposition: Option<ReviewDisposition>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ActionItemAddParams {
    /// Action item title. Equivalent to `gander action-items add --title <title>`.
    pub title: String,
    pub body: Option<String>,
    pub action: Option<ActionIntent>,
    /// Comment ids/prefixes to link. A todo comment may be linked to at most one open action item.
    pub comments: Option<Vec<String>>,
    pub path: Option<String>,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    pub symbol: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ActionItemEditParams {
    /// Full id or unambiguous id prefix.
    pub id: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub clear_body: Option<bool>,
    pub action: Option<ActionIntent>,
    pub clear_action: Option<bool>,
    pub path: Option<String>,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    pub symbol: Option<String>,
    pub clear_target: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ActionItemCommentsParams {
    pub id: String,
    pub comments: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ActionItemTicketParams {
    pub id: String,
    pub tracker: String,
    pub reference: String,
    pub url: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ActionItemRemoveTicketParams {
    pub id: String,
    pub reference: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ActionItemCloseParams {
    pub id: String,
    pub disposition: McpClosedDisposition,
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum McpClosedDisposition {
    Completed,
    Dismissed,
    Deferred,
}

impl From<McpClosedDisposition> for crate::state::ClosedDisposition {
    fn from(value: McpClosedDisposition) -> Self {
        match value {
            McpClosedDisposition::Completed => Self::Completed,
            McpClosedDisposition::Dismissed => Self::Dismissed,
            McpClosedDisposition::Deferred => Self::Deferred,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WalkthroughAddStepParams {
    /// Step title. Equivalent to `gander walkthrough add-step <title>`.
    pub title: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    pub symbol: Option<String>,
    pub why: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WalkthroughMoveStepParams {
    /// Full id or unambiguous id prefix. Equivalent to `gander walkthrough move-step <id> <to>`.
    pub id: String,
    pub to: usize,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct WalkthroughTargetParams {
    pub path: Option<String>,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    pub symbol: Option<String>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct WalkthroughArtifactParams {
    pub title: String,
    /// example | output | diagram | note
    pub kind: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct WalkthroughSetStepParams {
    pub id: Option<String>,
    /// step (default) | chapter
    pub kind: Option<String>,
    /// spotlight (default) | glance
    pub importance: Option<String>,
    pub change_id: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub why: Option<String>,
    pub target: Option<WalkthroughTargetParams>,
    #[serde(default)]
    pub extra_targets: Vec<WalkthroughTargetParams>,
    #[serde(default)]
    pub artifacts: Vec<WalkthroughArtifactParams>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct WalkthroughSetParams {
    pub title: Option<String>,
    /// Complete replacement ordered in normal-stream Spotlight order.
    pub steps: Vec<WalkthroughSetStepParams>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AttentionListParams {
    /// When true, list raw durable assignments; otherwise list effective current regions.
    pub assigned: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AttentionMutationParams {
    pub path: String,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    pub salience: Salience,
    pub rationale: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AttentionTargetParams {
    pub path: String,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    pub rationale: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AttentionClearParams {
    pub path: String,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AttentionAcknowledgeParams {
    pub path: Option<String>,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    /// Stable id returned by `attention_skim_fold_list`.
    pub fold_id: Option<String>,
    /// Acknowledge every current unacknowledged skim fold.
    pub all: Option<bool>,
}

#[tool_router]
impl GanderMcp {
    pub fn new(
        session_factory: impl FnOnce() -> ReviewSession + Send + 'static,
        jj: Option<Box<dyn JjBackend + Send>>,
        params: GanderMcpParams,
    ) -> Result<Self> {
        let workspace_root = params
            .workspace_root
            .canonicalize()
            .unwrap_or(params.workspace_root);
        let target_spec = review::SessionTargetSpec {
            repo: Some(workspace_root.display().to_string()),
            base: Some(params.target.base.clone()),
            revision: Some(params.target.rev.clone()),
            revset: Some(params.target.to_string()),
        };
        let mut handler = AcpHandler::new(params.overlay_path)?;
        if let Some(jj) = jj {
            handler.set_jj_backend(jj);
        }
        let (sender, receiver) = mpsc::channel::<SnapshotRequest>();
        // ReviewSession itself is not Send (interior caches), so it is
        // constructed on the thread that owns it.
        std::thread::spawn(move || {
            let mut session = session_factory();
            while let Ok(request) = receiver.recv() {
                // Pick up non-comment overlay suggestions written since the
                // last call.
                handler.refresh_overlay();
                let response = handler.handle_line(&mut session, &request.line);
                let _ = request.reply.send(response);
            }
        });
        Ok(Self {
            registry_dir: params.registry_dir,
            workspace_root,
            state_path: params.state_path,
            target: target_spec,
            diff_files: params.diff_files,
            attention_files: params.attention_files,
            generated_policy: params.generated_policy,
            ignore_globs: params.ignore_globs,
            initial_comment_state: params.initial_comment_state,
            agent_identity: params.agent_identity,
            snapshot: sender,
            tool_router: Self::tool_router(),
        })
    }

    #[tool(description = "Review target, repository, and a one-line summary of the change")]
    fn review_summary(&self) -> Result<CallToolResult, McpError> {
        self.call("review/summary", Value::Null)
    }

    #[tool(
        description = "Changed files with status, additions/deletions, viewed marks, and fingerprints"
    )]
    fn review_files(&self) -> Result<CallToolResult, McpError> {
        self.call("review/files", Value::Null)
    }

    #[tool(description = "Raw git-style diff for one file")]
    fn file_diff(
        &self,
        Parameters(params): Parameters<FileDiffParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call("review/file_diff", json!({ "path": params.path }))
    }

    #[tool(description = "Human review comments recorded in this session")]
    fn comments(
        &self,
        Parameters(params): Parameters<CommentsParams>,
    ) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        let state = self.load_state()?;
        let target = review::SessionTargetSpec {
            repo: Some(context.repo.display().to_string()),
            base: Some(context.base.clone()),
            revision: Some(context.revision.clone()),
            revset: Some(format!("{}..{}", context.base, context.revision)),
        };
        let active_session_id =
            review::find_session_for_target(&state, &target).map(|session| session.id.as_str());
        let channel = params.channel;
        let comments =
            review::list_comments_for_session(&state.comments, active_session_id, channel);
        json_result(crate::acp::comments_json(comments))
    }

    #[tool(
        description = "What the human reviewer is looking at right now: focused pane, selected file, and line/hunk when available"
    )]
    fn current_focus(&self) -> Result<CallToolResult, McpError> {
        self.call("review/current_focus", Value::Null)
    }

    #[tool(
        description = "Live TUI presentation status: active Spotlight index/count, Focus view, and current durable target"
    )]
    fn present_status(&self) -> Result<CallToolResult, McpError> {
        self.call("present/status", Value::Null)
    }

    #[tool(description = "Start Focus at the first durable Spotlight in the normal review stream")]
    fn present_start(&self) -> Result<CallToolResult, McpError> {
        self.call("present/start", Value::Null)
    }

    #[tool(
        description = "End live stream presentation and restore Focus when presentation owned it"
    )]
    fn present_end(&self) -> Result<CallToolResult, McpError> {
        self.call("present/end", Value::Null)
    }

    #[tool(description = "Advance to the next durable Spotlight in the normal stream")]
    fn present_next(&self) -> Result<CallToolResult, McpError> {
        self.call("present/next", Value::Null)
    }

    #[tool(description = "Move to the previous durable Spotlight in the normal stream")]
    fn present_prev(&self) -> Result<CallToolResult, McpError> {
        self.call("present/prev", Value::Null)
    }

    #[tool(
        description = "Jump stream presentation to a zero-based Spotlight index or durable step id"
    )]
    fn present_goto(
        &self,
        Parameters(params): Parameters<PresentGotoParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call(
            "present/goto",
            json!({ "index": params.index, "step_id": params.step_id }),
        )
    }

    #[tool(
        description = "Spotlight a path/line in the live TUI review view and optionally show a note"
    )]
    fn present_focus(
        &self,
        Parameters(params): Parameters<PresentFocusParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call("present/focus", json!({ "path": params.path, "line": params.line, "end_line": params.end_line, "note": params.note }))
    }

    #[tool(
        description = "Reload durable review/walkthrough state and re-anchor live stream presentation"
    )]
    fn present_reload(&self) -> Result<CallToolResult, McpError> {
        self.call("present/reload", Value::Null)
    }

    #[tool(
        description = "The jj changes in the current stack (trunk()..@), oldest first, with the reviewed change marked — the human often treats these as stacked PRs or logical groupings that flow into each other, so prefer organizing the review change-by-change when several exist"
    )]
    fn stack_changes(&self) -> Result<CallToolResult, McpError> {
        self.call("review/stack_changes", Value::Null)
    }

    #[tool(
        description = "Git-style diff of one jj change against its parent (change_id- .. change_id), useful for durable walkthrough and attention targets"
    )]
    fn change_diff(
        &self,
        Parameters(params): Parameters<ChangeDiffParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call(
            "review/change_diff",
            json!({ "change_id": params.change_id }),
        )
    }

    #[tool(
        description = "Suggest a review order (riskiest or most central files first); surfaced live in the reviewer's TUI"
    )]
    fn set_ordering(
        &self,
        Parameters(params): Parameters<SetOrderingParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call("review/set_ordering", json!({ "paths": params.paths }))
    }

    #[tool(
        description = "Flag a section that needs extra scrutiny; pinned in the reviewer's diff gutter"
    )]
    fn flag_section(
        &self,
        Parameters(params): Parameters<FlagSectionParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call(
            "review/flag_section",
            json!({
                "path": params.path,
                "line": params.line,
                "reason": params.reason,
                "priority": params.priority,
            }),
        )
    }

    #[tool(
        description = "Draft a review comment for the human to accept, edit, or discard in the TUI"
    )]
    fn draft_comment(
        &self,
        Parameters(params): Parameters<DraftCommentParams>,
    ) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        Self::ensure_selected_diff_file(&context, &params.path)?;
        let line = params.line.map(|line| line as usize);
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index_for_context(state, &context);
            let session_id = state.sessions[idx].id.clone();
            let anchor = context
                .files
                .iter()
                .find(|file| file.path == params.path)
                .and_then(|file| crate::anchor::comment_anchor_for_file_diff(file, line, line));
            let observation = crate::provenance::CommentObservation::new(
                Self::provenance_snapshot(&context, &state.sessions[idx]),
                anchor.clone(),
            );
            review::add_comment(
                &mut state.sessions[idx],
                &mut state.comments,
                review::NewComment {
                    session_id,
                    path: Some(params.path),
                    line,
                    end_line: None,
                    anchor,
                    observation: Some(observation),
                    body: params.body,
                    kind: None,
                    action: None,
                    state: CommentState::Draft,
                    author: this.agent_identity.clone(),
                    channel: Channel::Onboarding,
                },
            )
        })
    }

    #[tool(
        description = "Running gander review instances (workspace root, target, summary, last input time), most recently used first"
    )]
    fn list_reviews(&self) -> Result<CallToolResult, McpError> {
        let instances = registry::live_instances(&self.registry_dir);
        json_result(
            serde_json::to_value(instances)
                .map_err(|error| McpError::internal_error(error.to_string(), None))?,
        )
    }

    #[tool(description = "List durable review sessions. Equivalent to `gander reviews list`.")]
    fn reviews_list(&self) -> Result<CallToolResult, McpError> {
        let state = self.load_state()?;
        json_result(json!({ "sessions": review::list_sessions(&state) }))
    }

    #[tool(
        description = "Show one durable review session. Equivalent to `gander reviews show <id>`."
    )]
    fn reviews_show(
        &self,
        Parameters(params): Parameters<IdParams>,
    ) -> Result<CallToolResult, McpError> {
        let state = self.load_state()?;
        let session = review::find_session(&state, &params.id)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        json_result(to_value(session)?)
    }

    #[tool(
        description = "Create or reuse the durable review session for this MCP target. Equivalent to `gander reviews create`."
    )]
    fn reviews_create(
        &self,
        Parameters(params): Parameters<ReviewsCreateParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_state_mut(|state, this| {
            Ok(review::ensure_session(state, &this.target, params.title.as_deref()).clone())
        })
    }

    #[tool(
        description = "Show the active session-level team disposition. Equivalent to `gander reviews disposition show`."
    )]
    fn review_disposition(&self) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        let state = self.load_state()?;
        let target = review::SessionTargetSpec {
            repo: Some(context.repo.display().to_string()),
            base: Some(context.base),
            revision: Some(context.revision),
            revset: None,
        };
        let disposition =
            review::find_session_for_target(&state, &target).and_then(|s| s.disposition);
        json_result(json!({ "disposition": disposition }))
    }

    #[tool(
        description = "Set or clear the active session-level team disposition. Equivalent to `gander reviews disposition set|clear`."
    )]
    fn review_disposition_set(
        &self,
        Parameters(params): Parameters<ReviewDispositionParams>,
    ) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index_for_context(state, &context);
            Ok(review::set_session_disposition(
                &mut state.sessions[idx],
                params.disposition,
            ))
        })
    }

    #[tool(
        description = "Add a durable review comment. Equivalent to `gander comments add` (including `--channel`), except the CLI-only `[comments].default-channel` config fallback is not consulted; external additions merge into a running TUI."
    )]
    fn comment_add(
        &self,
        Parameters(params): Parameters<CommentAddParams>,
    ) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        let general = params.general.unwrap_or(false);
        match (params.path.as_deref(), general) {
            (Some(path), false) => Self::ensure_selected_diff_file(&context, path)?,
            (None, true) => {}
            _ => {
                return Err(McpError::invalid_params(
                    "provide exactly one of `path` or `general: true`",
                    None,
                ));
            }
        }
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index_for_context(state, &context);
            let session_id = state.sessions[idx].id.clone();
            let anchor = params.path.as_deref().and_then(|path| {
                context
                    .files
                    .iter()
                    .find(|file| file.path == path)
                    .and_then(|file| {
                        crate::anchor::comment_anchor_for_file_diff(
                            file,
                            params.line,
                            params.end_line,
                        )
                    })
            });
            let observation = crate::provenance::CommentObservation::new(
                Self::provenance_snapshot(&context, &state.sessions[idx]),
                anchor.clone(),
            );
            let initial_state = params
                .state
                .map(Into::into)
                .unwrap_or(this.initial_comment_state);
            // Same channel semantics as CLI `comments add --channel`: an
            // explicit channel wins, otherwise the state-derived default;
            // a non-actionable channel demotes a todo to a private draft.
            let channel = params.channel.unwrap_or({
                if initial_state == CommentState::Todo {
                    Channel::Delegation
                } else {
                    Channel::Note
                }
            });
            let initial_state = if initial_state == CommentState::Todo && !channel.permits_todo() {
                CommentState::Draft
            } else {
                initial_state
            };
            review::add_comment(
                &mut state.sessions[idx],
                &mut state.comments,
                review::NewComment {
                    session_id,
                    path: params.path,
                    line: params.line,
                    end_line: params.end_line,
                    anchor,
                    observation: Some(observation),
                    body: params.body,
                    kind: params.kind,
                    action: params.action,
                    state: initial_state,
                    author: this.agent_identity.clone(),
                    channel,
                },
            )
        })
    }

    #[tool(
        description = "Mark selected durable comments, or all active-session human-authored drafts, ready as todos. Equivalent to `gander comments ready`. Bulk `all_drafts` skips agent-authored drafts awaiting human triage (reported as `skipped_agent_drafts`); select an agent draft explicitly by id to ready it."
    )]
    fn comments_ready(
        &self,
        Parameters(params): Parameters<CommentsReadyParams>,
    ) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        let all_drafts = params.all_drafts.unwrap_or(false);
        let ids = params.ids.unwrap_or_default();
        if all_drafts != ids.is_empty() {
            return Err(McpError::invalid_params(
                "provide non-empty `ids` or `all_drafts: true`, but not both",
                None,
            ));
        }
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index_for_context(state, &context);
            if all_drafts {
                review::ready_all_drafts(&mut state.sessions[idx], &mut state.comments)
            } else {
                review::ready_selected_comments(&mut state.sessions[idx], &mut state.comments, &ids)
            }
        })
    }

    #[tool(
        description = "Resolve a durable review comment, optionally appending a reply first. Equivalent to `gander comments resolve <id> [--reply <text>]`. Timestamped comment updates and replies merge into a running TUI's review state."
    )]
    fn comment_resolve(
        &self,
        Parameters(params): Parameters<CommentResolveParams>,
    ) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index_for_context(state, &context);
            if let Some(reply) = params.reply {
                let snapshot = Self::provenance_snapshot(&context, &state.sessions[idx]);
                review::reply_and_maybe_resolve_comment(
                    &mut state.sessions[idx],
                    &mut state.comments,
                    &params.id,
                    reply,
                    this.agent_identity.clone(),
                    true,
                    snapshot,
                )
            } else {
                review::resolve_comment(&mut state.sessions[idx], &mut state.comments, &params.id)
            }
        })
    }

    #[tool(
        description = "Append a durable reply to a review comment. Equivalent to `gander comments reply <id> --body <text> [--resolve]`. Timestamped comment updates and replies merge into a running TUI's review state."
    )]
    fn comment_reply(
        &self,
        Parameters(params): Parameters<CommentReplyParams>,
    ) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index_for_context(state, &context);
            let snapshot = Self::provenance_snapshot(&context, &state.sessions[idx]);
            review::reply_and_maybe_resolve_comment(
                &mut state.sessions[idx],
                &mut state.comments,
                &params.id,
                params.body,
                this.agent_identity.clone(),
                params.resolve.unwrap_or(false),
                snapshot,
            )
        })
    }

    #[tool(
        description = "Set a durable review comment state. Equivalent to `gander comments set-state <id> --state <state>`; timestamped updates merge into a running TUI."
    )]
    fn comment_set_state(
        &self,
        Parameters(params): Parameters<CommentSetStateParams>,
    ) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index_for_context(state, &context);
            review::set_comment_state(
                &mut state.sessions[idx],
                &mut state.comments,
                &params.id,
                params.state,
            )
        })
    }

    #[tool(
        description = "Add a durable action item. Equivalent to `gander action-items add`; external additions merge into a running TUI."
    )]
    fn action_item_add(
        &self,
        Parameters(params): Parameters<ActionItemAddParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(path) = params.path.as_deref() {
            self.ensure_diff_file(path)?;
        }
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index(state);
            review::add_action_item(
                &mut state.sessions[idx],
                &state.comments,
                review::NewActionItem {
                    title: params.title,
                    body: params.body,
                    action: params.action,
                    comment_selectors: params.comments.unwrap_or_default(),
                    external_tickets: Vec::new(),
                    target: params.path.map(|file| StateReviewTarget {
                        file: Some(file),
                        line: params.line,
                        end_line: params.end_line,
                        symbol: params.symbol,
                        ..StateReviewTarget::default()
                    }),
                },
            )
        })
    }

    #[tool(description = "Edit a durable action item. Equivalent to `gander action-items edit`.")]
    fn action_item_edit(
        &self,
        Parameters(params): Parameters<ActionItemEditParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(path) = params.path.as_deref() {
            self.ensure_diff_file(path)?;
        }
        if params.clear_body.unwrap_or(false) && params.body.is_some() {
            return Err(McpError::invalid_params(
                "provide body or clear_body, not both",
                None,
            ));
        }
        if params.clear_action.unwrap_or(false) && params.action.is_some() {
            return Err(McpError::invalid_params(
                "provide action or clear_action, not both",
                None,
            ));
        }
        if params.clear_target.unwrap_or(false) && params.path.is_some() {
            return Err(McpError::invalid_params(
                "provide target fields or clear_target, not both",
                None,
            ));
        }
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index(state);
            let target = if params.clear_target.unwrap_or(false) {
                Some(None)
            } else {
                params.path.map(|file| {
                    Some(StateReviewTarget {
                        file: Some(file),
                        line: params.line,
                        end_line: params.end_line,
                        symbol: params.symbol,
                        ..StateReviewTarget::default()
                    })
                })
            };
            review::edit_action_item(
                &mut state.sessions[idx],
                &params.id,
                review::ActionItemEdits {
                    title: params.title,
                    body: params
                        .clear_body
                        .unwrap_or(false)
                        .then_some(None)
                        .or_else(|| params.body.map(Some)),
                    action: params
                        .clear_action
                        .unwrap_or(false)
                        .then_some(None)
                        .or_else(|| params.action.map(Some)),
                    target,
                },
            )
        })
    }

    #[tool(
        description = "Link comments to an action item. Equivalent to `gander action-items link-comment`."
    )]
    fn action_item_link_comment(
        &self,
        Parameters(params): Parameters<ActionItemCommentsParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.comments.is_empty() {
            return Err(McpError::invalid_params("comments must be non-empty", None));
        }
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index(state);
            review::link_comment(
                &mut state.sessions[idx],
                &state.comments,
                &params.id,
                &params.comments,
            )
        })
    }

    #[tool(
        description = "Unlink comments from an action item. Equivalent to `gander action-items unlink-comment`."
    )]
    fn action_item_unlink_comment(
        &self,
        Parameters(params): Parameters<ActionItemCommentsParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.comments.is_empty() {
            return Err(McpError::invalid_params("comments must be non-empty", None));
        }
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index(state);
            review::unlink_comment(
                &mut state.sessions[idx],
                &state.comments,
                &params.id,
                &params.comments,
            )
        })
    }

    #[tool(description = "Add an external ticket reference to an action item.")]
    fn action_item_add_ticket(
        &self,
        Parameters(params): Parameters<ActionItemTicketParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index(state);
            review::add_ticket(
                &mut state.sessions[idx],
                &params.id,
                review::NewExternalTicket {
                    tracker: params.tracker,
                    reference: params.reference,
                    url: params.url,
                },
            )
        })
    }

    #[tool(description = "Remove an external ticket reference from an action item.")]
    fn action_item_remove_ticket(
        &self,
        Parameters(params): Parameters<ActionItemRemoveTicketParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index(state);
            review::remove_ticket(&mut state.sessions[idx], &params.id, &params.reference)
        })
    }

    #[tool(description = "Close a durable action item. Equivalent to `gander action-items close`.")]
    fn action_item_close(
        &self,
        Parameters(params): Parameters<ActionItemCloseParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index(state);
            review::close_action_item(
                &mut state.sessions[idx],
                &params.id,
                params.disposition.into(),
                params.outcome,
            )
        })
    }

    #[tool(
        description = "Reopen a durable action item. Equivalent to `gander action-items reopen`."
    )]
    fn action_item_reopen(
        &self,
        Parameters(params): Parameters<IdParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index(state);
            review::reopen_action_item(&mut state.sessions[idx], &params.id)
        })
    }

    #[tool(
        description = "Delete a durable action item. Equivalent to `gander action-items delete`."
    )]
    fn action_item_delete(
        &self,
        Parameters(params): Parameters<IdParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index(state);
            review::delete_action_item(&mut state.sessions[idx], &params.id)
        })
    }

    #[tool(description = "List durable action items. Equivalent to `gander action-items list`.")]
    fn action_items_list(&self) -> Result<CallToolResult, McpError> {
        let mut state = self.load_state()?;
        let session = review::ensure_session(&mut state, &self.target, None).clone();
        json_result(json!({ "action_items": review::list_action_items(&session) }))
    }

    #[tool(
        description = "Replace the durable walkthrough used for normal-stream Spotlight ordering, chapter headers, narration cards, and agent attention. Equivalent to `gander walkthrough set`."
    )]
    fn walkthrough_set(
        &self,
        Parameters(params): Parameters<WalkthroughSetParams>,
    ) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        let files = self.attention_files_for_context(&context);
        let stack_change_ids = if params
            .steps
            .iter()
            .any(|step| step.kind.as_deref() == Some("chapter"))
        {
            self.selected_stack_change_ids()?
        } else {
            Vec::new()
        };
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index_for_context(state, &context);
            let steps = params
                .steps
                .into_iter()
                .map(|params| walkthrough_step_from_params(params, &this.agent_identity))
                .collect::<Result<Vec<_>>>()?;
            let prior = state.sessions[idx]
                .walkthroughs
                .first()
                .map(|walkthrough| walkthrough.steps.as_slice())
                .unwrap_or(&[]);
            let normalized = review::normalize_walkthrough_replacement(
                prior,
                params.title,
                steps,
                &this.agent_identity,
                &files,
                &stack_change_ids,
            )?;
            let warnings = normalized.warnings;
            let walkthrough = review::set_walkthrough(
                &mut state.sessions[idx],
                normalized.title,
                normalized.steps,
            );
            crate::attention::sync_agent_attention(&mut state.sessions[idx], &files)?;
            Ok(json!({ "walkthrough": walkthrough, "warnings": warnings }))
        })
    }

    #[tool(
        description = "Add a durable walkthrough step. Equivalent to `gander walkthrough add-step`; external additions merge into a running TUI."
    )]
    fn walkthrough_add_step(
        &self,
        Parameters(params): Parameters<WalkthroughAddStepParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(file) = params.file.as_deref() {
            self.ensure_diff_file(file)?;
        }
        let context = self.selected_review_context()?;
        let files = self.attention_files_for_context(&context);
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index_for_context(state, &context);
            let mut target = params
                .file
                .as_deref()
                .and_then(|file| {
                    crate::attention::target_for_diff(&files, file, params.line, params.end_line)
                        .ok()
                })
                .unwrap_or_else(|| StateReviewTarget {
                    file: params.file.clone(),
                    line: params.line,
                    end_line: params.end_line,
                    ..StateReviewTarget::default()
                });
            target.symbol = params.symbol;
            let step = review::add_walkthrough_step(
                &mut state.sessions[idx],
                WalkthroughStep {
                    id: String::new(),
                    author: Some(this.agent_identity.clone()),
                    title: Some(params.title),
                    body: params.body,
                    why: params.why,
                    target,
                    ..WalkthroughStep::default()
                },
            );
            crate::attention::sync_agent_attention(&mut state.sessions[idx], &files)?;
            Ok(step)
        })
    }

    #[tool(
        description = "Remove a durable walkthrough step. Equivalent to `gander walkthrough remove-step <id>`."
    )]
    fn walkthrough_remove_step(
        &self,
        Parameters(params): Parameters<IdParams>,
    ) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        let files = self.attention_files_for_context(&context);
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index_for_context(state, &context);
            let step = review::remove_walkthrough_step(&mut state.sessions[idx], &params.id)?;
            crate::attention::sync_agent_attention(&mut state.sessions[idx], &files)?;
            Ok(step)
        })
    }

    #[tool(
        description = "Move a durable walkthrough step. Equivalent to `gander walkthrough move-step <id> --to <index>`; timestamped updates merge into a running TUI."
    )]
    fn walkthrough_move_step(
        &self,
        Parameters(params): Parameters<WalkthroughMoveStepParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index(state);
            review::move_walkthrough_step(&mut state.sessions[idx], &params.id, params.to)
        })
    }

    #[tool(description = "Show durable walkthroughs. Equivalent to `gander walkthrough show`.")]
    fn walkthrough_show(&self) -> Result<CallToolResult, McpError> {
        let mut state = self.load_state()?;
        let session = review::ensure_session(&mut state, &self.target, None).clone();
        json_result(json!({ "walkthroughs": session.walkthroughs }))
    }

    #[tool(
        description = "List effective attention regions or raw durable assignments. Equivalent to `gander attention list [--mode effective|assigned]`."
    )]
    fn attention_list(
        &self,
        Parameters(params): Parameters<AttentionListParams>,
    ) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        let files = self.attention_files_for_context(&context);
        let state = self.load_state()?;
        let target = Self::target_for_context(&context);
        let session = review::find_session_for_target(&state, &target)
            .cloned()
            .unwrap_or_default();
        if params.assigned.unwrap_or(false) {
            json_result(json!({
                "mode": "assigned",
                "default_salience": "supporting",
                "regions": crate::attention::list_assigned_attention(&session, &files),
            }))
        } else {
            json_result(json!({
                "mode": "effective",
                "default_salience": "supporting",
                "regions": crate::attention::list_effective_attention(&session, &files),
            }))
        }
    }

    #[tool(
        description = "Show fingerprint-guarded attention coverage. Equivalent to `gander attention coverage show`."
    )]
    fn attention_coverage_show(&self) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        let files = self.attention_files_for_context(&context);
        let state = self.load_state()?;
        let target = Self::target_for_context(&context);
        let session = review::find_session_for_target(&state, &target)
            .cloned()
            .unwrap_or_default();
        json_result(to_value(crate::attention::attention_coverage(
            &session, &files,
        ))?)
    }

    #[tool(
        description = "List current review-stream skim folds and stale skim history. Equivalent to `gander attention skim-fold list`."
    )]
    fn attention_skim_fold_list(&self) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        let files = self.attention_files_for_context(&context);
        let state = self.load_state()?;
        let target = Self::target_for_context(&context);
        let session = review::find_session_for_target(&state, &target)
            .cloned()
            .unwrap_or_default();
        let folds = crate::attention::list_skim_folds(&session, &files, true);
        json_result(json!({
            "current": folds.iter().filter(|fold| fold.current).count(),
            "stale": folds.iter().filter(|fold| fold.stale).count(),
            "folds": folds,
        }))
    }

    #[tool(
        description = "Acknowledge one explicit current skim target/stable id or all current skims. Equivalent to `gander attention acknowledge`."
    )]
    fn attention_skim_fold_acknowledge(
        &self,
        Parameters(params): Parameters<AttentionAcknowledgeParams>,
    ) -> Result<CallToolResult, McpError> {
        let all = params.all.unwrap_or(false);
        let has_coordinates = params.line.is_some() || params.end_line.is_some();
        if all && (params.path.is_some() || params.fold_id.is_some() || has_coordinates) {
            return Err(McpError::invalid_params(
                "all=true cannot be combined with path, line, end_line, or fold_id".to_owned(),
                None,
            ));
        }
        if params.fold_id.is_some() && (params.path.is_some() || has_coordinates) {
            return Err(McpError::invalid_params(
                "fold_id cannot be combined with path, line, or end_line".to_owned(),
                None,
            ));
        }
        if params.path.is_none() && params.line.is_some() {
            return Err(McpError::invalid_params(
                "line requires path".to_owned(),
                None,
            ));
        }
        if params.end_line.is_some() && params.line.is_none() {
            return Err(McpError::invalid_params(
                "end_line requires line".to_owned(),
                None,
            ));
        }
        if params.line == Some(0) || params.end_line == Some(0) {
            return Err(McpError::invalid_params(
                "line numbers are 1-indexed".to_owned(),
                None,
            ));
        }
        if let (Some(start), Some(end)) = (params.line, params.end_line)
            && end < start
        {
            return Err(McpError::invalid_params(
                "end_line must be greater than or equal to line".to_owned(),
                None,
            ));
        }
        let selection = match (all, params.fold_id, params.path) {
            (true, None, None) => crate::attention::SkimSelection::AllCurrent,
            (false, Some(id), None) if !id.trim().is_empty() => {
                crate::attention::SkimSelection::StableId(id)
            }
            (false, None, Some(path)) => crate::attention::SkimSelection::Target {
                path,
                line: params.line,
                end_line: params.end_line,
            },
            _ => {
                return Err(McpError::invalid_params(
                    "provide exactly one of path, fold_id, or all=true".to_owned(),
                    None,
                ));
            }
        };
        let context = self.selected_review_context()?;
        let files = self.attention_files_for_context(&context);
        let mut state = self.load_state()?;
        let idx = self.ensure_session_index_for_context(&mut state, &context);
        let outcome =
            crate::attention::acknowledge_skim_folds(&mut state.sessions[idx], &files, &selection)
                .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
        crate::attention::apply_whole_file_viewed_effects(
            &mut state,
            &files,
            &outcome.whole_files_viewed,
        );
        state
            .save(&self.state_path)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        json_result(to_value(outcome)?)
    }

    #[tool(
        description = "Set an explicit durable human attention override. Equivalent to `gander attention set`."
    )]
    fn attention_set(
        &self,
        Parameters(params): Parameters<AttentionMutationParams>,
    ) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        let files = self.attention_files_for_context(&context);
        let target =
            crate::attention::target_for_diff(&files, &params.path, params.line, params.end_line)
                .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index_for_context(state, &context);
            let region = crate::attention::set_human_attention(
                &mut state.sessions[idx],
                target,
                params.salience,
                params.rationale,
            )?;
            Ok(crate::attention::assigned_attention_region(&region, &files))
        })
    }

    #[tool(
        description = "Clear the exact durable human attention override. Equivalent to `gander attention clear`."
    )]
    fn attention_clear(
        &self,
        Parameters(params): Parameters<AttentionClearParams>,
    ) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        let target = crate::attention::identity_target(&params.path, params.line, params.end_line)
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index_for_context(state, &context);
            Ok(json!({
                "cleared": crate::attention::clear_human_attention(
                    &mut state.sessions[idx],
                    &target,
                ),
                "target": target,
            }))
        })
    }

    #[tool(
        description = "Promote effective attention one step as a durable human override. Equivalent to `gander attention promote`."
    )]
    fn attention_promote(
        &self,
        Parameters(params): Parameters<AttentionTargetParams>,
    ) -> Result<CallToolResult, McpError> {
        self.mutate_attention_step(params, true)
    }

    #[tool(
        description = "Demote effective attention one step as a durable human override. Equivalent to `gander attention demote`."
    )]
    fn attention_demote(
        &self,
        Parameters(params): Parameters<AttentionTargetParams>,
    ) -> Result<CallToolResult, McpError> {
        self.mutate_attention_step(params, false)
    }

    #[tool(
        description = "Seed missing lockfile/generated/ignore-policy Skim assignments. Equivalent to `gander attention seed-heuristics`."
    )]
    fn attention_seed_heuristics(&self) -> Result<CallToolResult, McpError> {
        self.update_attention_heuristics(false)
    }

    #[tool(
        description = "Recompute heuristics while preserving fingerprint-drifted stale regions. Equivalent to `gander attention recompute-heuristics`."
    )]
    fn attention_recompute_heuristics(&self) -> Result<CallToolResult, McpError> {
        self.update_attention_heuristics(true)
    }

    /// Dispatch one ACP request: through the live instance socket when this
    /// workspace has a running TUI, else against the snapshot session.
    fn call(&self, method: &str, params: Value) -> Result<CallToolResult, McpError> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        })
        .to_string();
        let response = self.dispatch_selected(&request)?;
        if let Some(error) = response.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                message.to_owned(),
            )]));
        }
        json_result(response.get("result").cloned().unwrap_or(Value::Null))
    }

    fn dispatch_selected(&self, request: &str) -> Result<Value, McpError> {
        match self.call_live(request) {
            Some(response) => response,
            None => self.call_snapshot(request),
        }
    }

    fn selected_review_context(&self) -> Result<SelectedReviewContext, McpError> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "review/provenance_context",
            "params": Value::Null,
        })
        .to_string();
        let response = self.dispatch_selected(&request)?;
        if let Some(error) = response.get("error") {
            return Err(McpError::internal_error(
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("failed to read selected review context")
                    .to_owned(),
                None,
            ));
        }
        let mut context: SelectedReviewContext =
            serde_json::from_value(response.get("result").cloned().unwrap_or(Value::Null))
                .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        context.repo = context.repo.canonicalize().unwrap_or(context.repo);
        Ok(context)
    }

    fn selected_stack_change_ids(&self) -> Result<Vec<String>, McpError> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "review/stack_changes",
            "params": Value::Null,
        })
        .to_string();
        let response = self.dispatch_selected(&request)?;
        if let Some(error) = response.get("error") {
            return Err(McpError::internal_error(
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("failed to read current stack")
                    .to_owned(),
                None,
            ));
        }
        Ok(response["result"]["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|change| change["change_id"].as_str().map(str::to_owned))
            .collect())
    }

    /// `None` when no live instance serves this workspace; the caller then
    /// falls back to the snapshot.
    #[cfg(unix)]
    fn call_live(&self, request: &str) -> Option<Result<Value, McpError>> {
        use std::io::{BufRead, BufReader, Write};

        let instance = registry::find_live_for_workspace(&self.registry_dir, &self.workspace_root)?;
        if instance.base != self.target.base.as_deref().unwrap_or_default()
            || instance.rev != self.target.revision.as_deref().unwrap_or_default()
        {
            eprintln!(
                "warning: bridging to live TUI session reviewing {}..{}; requested {} ignored",
                instance.base,
                instance.rev,
                self.target.revset.as_deref().unwrap_or("requested target")
            );
        }
        let call = || -> std::io::Result<Value> {
            let mut stream = std::os::unix::net::UnixStream::connect(&instance.socket_path)?;
            stream.write_all(request.as_bytes())?;
            stream.write_all(b"\n")?;
            stream.flush()?;
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line)?;
            serde_json::from_str(&line).map_err(std::io::Error::other)
        };
        Some(call().map_err(|error| {
            McpError::internal_error(
                format!(
                    "failed to reach live gander instance (pid {}): {error}",
                    instance.pid
                ),
                None,
            )
        }))
    }

    #[cfg(not(unix))]
    fn call_live(&self, _request: &str) -> Option<Result<Value, McpError>> {
        None
    }

    fn call_snapshot(&self, request: &str) -> Result<Value, McpError> {
        let (reply_sender, reply_receiver) = mpsc::channel();
        self.snapshot
            .send(SnapshotRequest {
                line: request.to_owned(),
                reply: reply_sender,
            })
            .map_err(|_| McpError::internal_error("snapshot session thread exited", None))?;
        reply_receiver
            .recv()
            .map_err(|_| McpError::internal_error("snapshot session thread exited", None))?
            .ok_or_else(|| McpError::internal_error("no response from review session", None))
    }

    fn load_state(&self) -> Result<ReviewState, McpError> {
        ReviewState::load_or_default(&self.state_path)
            .map_err(|error| McpError::internal_error(error.to_string(), None))
    }

    fn target_for_context(context: &SelectedReviewContext) -> review::SessionTargetSpec {
        review::SessionTargetSpec {
            repo: Some(context.repo.display().to_string()),
            base: Some(context.base.clone()),
            revision: Some(context.revision.clone()),
            revset: Some(format!("{}..{}", context.base, context.revision)),
        }
    }

    fn attention_files_for_context(&self, context: &SelectedReviewContext) -> Vec<FileDiff> {
        let startup_target = self.target.base.as_deref() == Some(context.base.as_str())
            && self.target.revision.as_deref() == Some(context.revision.as_str());
        let mut files = context.files.clone();
        if startup_target {
            // The live/snapshot provenance context is authoritative for visible
            // files. Add only startup files removed by ignore policy so those
            // can still seed Skim; never replace a current live fingerprint.
            let current_paths = files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            let ignored = self
                .attention_files
                .iter()
                .filter(|file| {
                    !self.diff_files.contains(&file.path)
                        && !current_paths.contains(file.path.as_str())
                })
                .cloned()
                .collect::<Vec<_>>();
            files.extend(ignored);
        }
        files
    }

    fn mutate_attention_step(
        &self,
        params: AttentionTargetParams,
        promote: bool,
    ) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        let files = self.attention_files_for_context(&context);
        let target =
            crate::attention::target_for_diff(&files, &params.path, params.line, params.end_line)
                .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index_for_context(state, &context);
            let region = if promote {
                crate::attention::promote_human_attention(
                    &mut state.sessions[idx],
                    target,
                    params.rationale,
                    &files,
                )
            } else {
                crate::attention::demote_human_attention(
                    &mut state.sessions[idx],
                    target,
                    params.rationale,
                    &files,
                )
            }?;
            Ok(crate::attention::assigned_attention_region(&region, &files))
        })
    }

    fn update_attention_heuristics(&self, recompute: bool) -> Result<CallToolResult, McpError> {
        let context = self.selected_review_context()?;
        let files = self.attention_files_for_context(&context);
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index_for_context(state, &context);
            crate::attention::sync_agent_attention(&mut state.sessions[idx], &files)?;
            let update = crate::attention::update_heuristic_attention(
                &mut state.sessions[idx],
                &files,
                &this.generated_policy,
                &this.ignore_globs,
                recompute,
            )?;
            Ok(json!({
                "mode": if recompute { "recompute" } else { "seed" },
                "update": update,
                "regions": crate::attention::list_assigned_attention(
                    &state.sessions[idx],
                    &files,
                ),
            }))
        })
    }

    fn with_state_mut<T: Serialize>(
        &self,
        f: impl FnOnce(&mut ReviewState, &Self) -> Result<T>,
    ) -> Result<CallToolResult, McpError> {
        let mut state = self.load_state()?;
        let value = f(&mut state, self)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        state
            .save(&self.state_path)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        json_result(to_value(value)?)
    }

    fn ensure_session_index(&self, state: &mut ReviewState) -> usize {
        let id = review::ensure_session(state, &self.target, None).id.clone();
        state
            .sessions
            .iter()
            .position(|session| session.id == id)
            .unwrap()
    }

    fn ensure_session_index_for_context(
        &self,
        state: &mut ReviewState,
        context: &SelectedReviewContext,
    ) -> usize {
        let target = review::SessionTargetSpec {
            repo: Some(context.repo.display().to_string()),
            base: Some(context.base.clone()),
            revision: Some(context.revision.clone()),
            revset: Some(format!("{}..{}", context.base, context.revision)),
        };
        let id = review::ensure_session(state, &target, None).id.clone();
        state
            .sessions
            .iter()
            .position(|session| session.id == id)
            .unwrap()
    }

    fn ensure_diff_file(&self, path: &str) -> Result<(), McpError> {
        if self.diff_files.iter().any(|file| file == path) {
            Ok(())
        } else {
            Err(McpError::invalid_params(
                format!("`{path}` is not a file in the current diff"),
                None,
            ))
        }
    }

    fn ensure_selected_diff_file(
        context: &SelectedReviewContext,
        path: &str,
    ) -> Result<(), McpError> {
        if context.files.iter().any(|file| file.path == path) {
            Ok(())
        } else {
            Err(McpError::invalid_params(
                format!("`{path}` is not a file in the current diff"),
                None,
            ))
        }
    }

    fn provenance_snapshot(
        context: &SelectedReviewContext,
        session: &crate::state::ReviewSession,
    ) -> crate::provenance::SnapshotEvidence {
        crate::provenance::SnapshotEvidence::capture(
            chrono::Utc::now(),
            session.id.clone(),
            session.target.clone(),
            context.files.iter(),
        )
    }
}

fn walkthrough_step_from_params(
    params: WalkthroughSetStepParams,
    author: &Identity,
) -> Result<WalkthroughStep> {
    let kind = match params.kind.as_deref().unwrap_or("step") {
        "step" => StepKind::Step,
        "chapter" => StepKind::Chapter,
        value => color_eyre::eyre::bail!("invalid walkthrough kind `{value}`"),
    };
    let importance = match params.importance.as_deref().unwrap_or("spotlight") {
        "spotlight" => StepImportance::Spotlight,
        "glance" => StepImportance::Glance,
        value => color_eyre::eyre::bail!("invalid walkthrough importance `{value}`"),
    };
    if kind == StepKind::Chapter
        && params
            .change_id
            .as_deref()
            .is_none_or(|change_id| change_id.trim().is_empty())
    {
        color_eyre::eyre::bail!("chapter walkthrough steps require change_id");
    }
    let target = walkthrough_target_from_params(params.target)?;
    let extra_targets = params
        .extra_targets
        .into_iter()
        .map(|target| walkthrough_target_from_params(Some(target)))
        .collect::<Result<Vec<_>>>()?;
    let artifacts = params
        .artifacts
        .into_iter()
        .map(|artifact| {
            let kind = match artifact.kind.as_deref().unwrap_or("example") {
                "example" => StepArtifactKind::Example,
                "output" => StepArtifactKind::Output,
                "diagram" => StepArtifactKind::Diagram,
                "note" => StepArtifactKind::Note,
                value => color_eyre::eyre::bail!("invalid walkthrough artifact kind `{value}`"),
            };
            if artifact.title.trim().is_empty() || artifact.body.trim().is_empty() {
                color_eyre::eyre::bail!("walkthrough artifacts require non-empty title and body");
            }
            Ok(StepArtifact {
                title: artifact.title,
                kind,
                body: artifact.body,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(WalkthroughStep {
        id: params.id.unwrap_or_default(),
        author: Some(author.clone()),
        target,
        importance,
        kind,
        change_id: params.change_id,
        title: params.title,
        body: params.body,
        why: params.why,
        artifacts,
        extra_targets,
        updated_at: None,
    })
}

fn walkthrough_target_from_params(
    params: Option<WalkthroughTargetParams>,
) -> Result<StateReviewTarget> {
    let Some(params) = params else {
        return Ok(StateReviewTarget::default());
    };
    if let (Some(start), Some(end)) = (params.line, params.end_line)
        && end < start
    {
        color_eyre::eyre::bail!("walkthrough end_line must be greater than or equal to line");
    }
    let mut target = StateReviewTarget {
        file: params.path,
        line: params.line,
        end_line: params.end_line,
        ..StateReviewTarget::default()
    };
    target.symbol = params.symbol;
    Ok(target)
}

fn to_value(value: impl Serialize) -> Result<Value, McpError> {
    serde_json::to_value(value).map_err(|error| McpError::internal_error(error.to_string(), None))
}

fn json_result(value: Value) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string_pretty(&value)
        .map_err(|error| McpError::internal_error(error.to_string(), None))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GanderMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("gander", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "gander hosts a code review of a jj change for a human reviewer. \
                 Read the change with review_summary, review_files, and file_diff; \
                 call stack_changes early — the human often reviews a stack of jj \
                 changes like stacked PRs, and change_diff reads one change \
                 against its parent. See what the human is looking at with \
                 current_focus; then help organize the review with set_ordering, \
                 flag_section, durable walkthrough_set/walkthrough_add_step/walkthrough_show, \
                 attention_set/attention_list, and attention_seed_heuristics. \
                 Use 3-7 precise Spotlight steps with teaching `why`/`body` \
                 narration for the mental-model delta; use Skim attention for \
                 generated, lockfile, and routine churn. Durable targets \
                 re-anchor or become stale by fingerprint and render in the \
                 normal review stream. Use draft_comment for concrete issues — \
                 suggestions appear live in the \
                 reviewer's terminal, and drafted comments are triaged by the \
                 human. The review refreshes automatically as new changes land. \
                 list_reviews shows every running review instance. Do not modify \
                 the repository.",
            )
    }
}

/// Serve MCP on stdio until the client disconnects. The factory builds the
/// snapshot fallback session on its owning thread.
pub fn run(
    session_factory: impl FnOnce() -> ReviewSession + Send + 'static,
    jj: Option<Box<dyn JjBackend + Send>>,
    params: GanderMcpParams,
) -> Result<()> {
    let server = GanderMcp::new(session_factory, jj, params)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .wrap_err("failed to start the MCP runtime")?;
    runtime.block_on(async move {
        let service = server
            .serve(stdio())
            .await
            .wrap_err("failed to serve MCP on stdio")?;
        service
            .waiting()
            .await
            .wrap_err("MCP server terminated abnormally")?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_schema_exposes_durable_curation_and_no_removed_chunk_tools() {
        let names = GanderMcp::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect::<Vec<_>>();
        for current in [
            "walkthrough_add_step",
            "walkthrough_set",
            "walkthrough_show",
            "attention_set",
            "attention_list",
            "attention_seed_heuristics",
        ] {
            assert!(names.iter().any(|name| name == current), "{current}");
        }
        for removed in [
            "set_chunks",
            "update_chunks",
            "remove_chunks",
            "set_change_briefs",
        ] {
            assert!(!names.iter().any(|name| name == removed), "{removed}");
        }
    }
    use std::path::Path;

    use crate::{diff::DiffSet, jj::ReviewTarget, state::ReviewState};

    fn session(dir: &Path) -> ReviewSession {
        let diff = DiffSet::parse(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        ReviewSession::new(
            dir.to_path_buf(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        )
    }

    fn attention_files() -> Vec<FileDiff> {
        DiffSet::parse(
            "diff --git a/src/app.rs b/src/app.rs\n--- a/src/app.rs\n+++ b/src/app.rs\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap()
        .files
    }

    fn server(dir: &Path) -> GanderMcp {
        let root = dir.to_path_buf();
        GanderMcp::new(
            move || session(&root),
            None,
            GanderMcpParams {
                overlay_path: dir.join("agent.json"),
                state_path: dir.join("state.json"),
                registry_dir: dir.join("registry"),
                workspace_root: dir.to_path_buf(),
                target: ReviewTarget::trunk_to_current(),
                diff_files: vec!["src/app.rs".into()],
                attention_files: attention_files(),
                generated_policy: GeneratedPolicy::default(),
                ignore_globs: Vec::new(),
                initial_comment_state: CommentState::Todo,
                agent_identity: Identity::agent(),
            },
        )
        .unwrap()
    }

    struct StackBackend;

    impl JjBackend for StackBackend {
        fn diff(&self, _repo: &Path, _target: &ReviewTarget) -> Result<String> {
            Ok(String::new())
        }

        fn change_summaries(&self, _repo: &Path) -> Result<Vec<crate::jj::JjChangeSummary>> {
            Ok(Vec::new())
        }

        fn stack_changes(
            &self,
            _repo: &Path,
            _target: &ReviewTarget,
        ) -> Result<Vec<crate::jj::JjChangeSummary>> {
            Ok(vec![crate::jj::JjChangeSummary {
                change_id: "abcdef".into(),
                bookmarks: String::new(),
                description: "chapter".into(),
            }])
        }

        fn snapshot_working_copy(&self, _repo: &Path) -> Result<()> {
            Ok(())
        }

        fn change_fingerprint(&self, _repo: &Path, _target: &ReviewTarget) -> Result<String> {
            Ok(String::new())
        }

        fn operations(&self, _repo: &Path) -> Result<Vec<crate::jj::JjOperationSummary>> {
            Ok(Vec::new())
        }

        fn diff_at_operation(
            &self,
            _repo: &Path,
            _target: &ReviewTarget,
            _operation_id: &str,
        ) -> Result<String> {
            Ok(String::new())
        }

        fn file_contents(&self, _repo: &Path, _rev: &str, _path: &str) -> Result<String> {
            Ok(String::new())
        }

        fn run_command(&self, _repo: &Path, _args: &[String]) -> Result<String> {
            Ok(String::new())
        }
    }

    fn server_with_stack(dir: &Path) -> GanderMcp {
        let root = dir.to_path_buf();
        GanderMcp::new(
            move || session(&root),
            Some(Box::new(StackBackend)),
            GanderMcpParams {
                overlay_path: dir.join("agent.json"),
                state_path: dir.join("state.json"),
                registry_dir: dir.join("registry"),
                workspace_root: dir.to_path_buf(),
                target: ReviewTarget::trunk_to_current(),
                diff_files: vec!["src/app.rs".into()],
                attention_files: attention_files(),
                generated_policy: GeneratedPolicy::default(),
                ignore_globs: Vec::new(),
                initial_comment_state: CommentState::Todo,
                agent_identity: Identity::agent(),
            },
        )
        .unwrap()
    }

    fn result_json(result: &CallToolResult) -> Value {
        assert_ne!(result.is_error, Some(true), "tool errored: {result:?}");
        let ContentBlock::Text(text) = &result.content[0] else {
            panic!("expected text content");
        };
        serde_json::from_str(&text.text).unwrap()
    }

    fn expected_comment_json(id: &str, state: &str, channel: &str, author_name: &str) -> Value {
        json!({
            "id": id,
            "session_id": "active",
            "path": null,
            "line": null,
            "end_line": null,
            "anchor": null,
            "observation": null,
            "body": format!("{id} body"),
            "kind": null,
            "action": null,
            "state": state,
            "author": { "kind": "human", "name": author_name },
            "channel": channel,
            "replies": [],
            "created_at": "2026-07-19T12:00:00Z",
            "updated_at": null
        })
    }

    #[test]
    fn tools_route_to_snapshot_session_without_live_instance() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());

        let summary = result_json(&server.review_summary().unwrap());
        assert_eq!(summary["base"], "trunk()");

        let files = result_json(&server.review_files().unwrap());
        assert_eq!(files[0]["path"], "src/app.rs");

        let diff = result_json(
            &server
                .file_diff(Parameters(FileDiffParams {
                    path: "src/app.rs".to_owned(),
                }))
                .unwrap(),
        );
        assert!(diff["raw"].as_str().unwrap().contains("+new"));

        let focus = result_json(&server.current_focus().unwrap());
        assert_eq!(focus["path"], "src/app.rs");
    }

    #[test]
    fn comment_mutations_are_immediately_visible_through_mcp_comment_reads() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        let draft = result_json(
            &server
                .draft_comment(Parameters(DraftCommentParams {
                    path: "src/app.rs".into(),
                    line: Some(1),
                    body: "durable draft".into(),
                }))
                .unwrap(),
        );
        let id = draft["id"].as_str().unwrap().to_owned();

        let comments = result_json(
            &server
                .comments(Parameters(CommentsParams { channel: None }))
                .unwrap(),
        );
        assert_eq!(comments[0]["id"], id);
        assert_eq!(comments[0]["state"], "draft");
        assert_eq!(comments[0]["channel"], "onboarding");
        assert_eq!(comments[0]["author"]["kind"], "agent");

        server
            .comments_ready(Parameters(CommentsReadyParams {
                ids: Some(vec![id.clone()]),
                all_drafts: None,
            }))
            .unwrap();
        let comments = result_json(
            &server
                .comments(Parameters(CommentsParams { channel: None }))
                .unwrap(),
        );
        assert_eq!(comments[0]["state"], "todo");
        assert_eq!(comments[0]["channel"], "delegation");
        let delegation = result_json(
            &server
                .comments(Parameters(CommentsParams {
                    channel: Some(Channel::Delegation),
                }))
                .unwrap(),
        );
        assert_eq!(delegation[0]["id"], id);
        let onboarding = result_json(
            &server
                .comments(Parameters(CommentsParams {
                    channel: Some(Channel::Onboarding),
                }))
                .unwrap(),
        );
        assert!(onboarding.as_array().unwrap().is_empty());

        server
            .comment_reply(Parameters(CommentReplyParams {
                id: id.clone(),
                body: "agent response".into(),
                resolve: None,
            }))
            .unwrap();
        let comments = result_json(
            &server
                .comments(Parameters(CommentsParams { channel: None }))
                .unwrap(),
        );
        assert_eq!(comments[0]["replies"][0]["body"], "agent response");
        assert_eq!(comments[0]["replies"][0]["author"]["kind"], "agent");

        server
            .comment_resolve(Parameters(CommentResolveParams { id, reply: None }))
            .unwrap();
        let comments = result_json(
            &server
                .comments(Parameters(CommentsParams { channel: None }))
                .unwrap(),
        );
        assert_eq!(comments[0]["state"], "resolved");
    }

    #[test]
    fn mcp_comments_filter_multiple_channels_and_preserve_exact_unfiltered_shape_order() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        let created_at = "2026-07-19T12:00:00Z".parse().unwrap();
        let mut state = ReviewState::default();
        state.sessions.push(crate::state::ReviewSession {
            id: "active".into(),
            target: StateReviewTarget {
                repo: Some(dir.path().canonicalize().unwrap().display().to_string()),
                base: Some("trunk()".into()),
                revision: Some("@".into()),
                ..Default::default()
            },
            ..Default::default()
        });
        state.comments.extend([
            crate::state::Comment {
                id: "onboarding".into(),
                session_id: Some("active".into()),
                body: "onboarding body".into(),
                state: CommentState::Draft,
                author: Identity {
                    kind: crate::state::AuthorKind::Human,
                    name: "First".into(),
                },
                channel: Channel::Onboarding,
                created_at,
                ..Default::default()
            },
            crate::state::Comment {
                id: "delegation".into(),
                session_id: Some("active".into()),
                body: "delegation body".into(),
                state: CommentState::Todo,
                author: Identity {
                    kind: crate::state::AuthorKind::Human,
                    name: "Second".into(),
                },
                channel: Channel::Delegation,
                created_at,
                ..Default::default()
            },
            crate::state::Comment {
                id: "collaboration".into(),
                session_id: Some("active".into()),
                body: "collaboration body".into(),
                state: CommentState::Resolved,
                author: Identity {
                    kind: crate::state::AuthorKind::Human,
                    name: "Third".into(),
                },
                channel: Channel::Collaboration,
                created_at,
                ..Default::default()
            },
        ]);
        state.save(&server.state_path).unwrap();

        let unfiltered = result_json(
            &server
                .comments(Parameters(CommentsParams { channel: None }))
                .unwrap(),
        );
        assert_eq!(
            unfiltered,
            json!([
                expected_comment_json("onboarding", "draft", "onboarding", "First"),
                expected_comment_json("delegation", "todo", "delegation", "Second"),
                expected_comment_json("collaboration", "resolved", "collaboration", "Third")
            ])
        );

        let filtered = result_json(
            &server
                .comments(Parameters(CommentsParams {
                    channel: Some(Channel::Collaboration),
                }))
                .unwrap(),
        );
        assert_eq!(
            filtered,
            json!([expected_comment_json(
                "collaboration",
                "resolved",
                "collaboration",
                "Third"
            )])
        );
    }

    #[test]
    fn mcp_comment_read_defaults_legacy_annotation_fields() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        std::fs::write(
            &server.state_path,
            r#"{"comments":[{"id":"legacy","body":"legacy","state":"todo","created_at":"2026-01-01T00:00:00Z","replies":[{"id":"reply","body":"done","created_at":"2026-01-01T00:01:00Z"}]}]}"#,
        )
        .unwrap();

        let comments = result_json(
            &server
                .comments(Parameters(CommentsParams { channel: None }))
                .unwrap(),
        );

        assert_eq!(
            comments[0]["author"],
            json!({ "kind": "human", "name": "local" })
        );
        assert_eq!(comments[0]["channel"], "delegation");
        assert_eq!(
            comments[0]["replies"][0]["author"],
            json!({ "kind": "human", "name": "local" })
        );
    }

    #[test]
    fn acp_dispatch_errors_become_tool_errors() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());

        let result = server
            .file_diff(Parameters(FileDiffParams {
                path: "nope.rs".to_owned(),
            }))
            .unwrap();

        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn removed_curation_method_invocations_become_mcp_errors() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        for method in [
            "review/set_chunks",
            "review/update_chunks",
            "review/remove_chunks",
            "review/set_change_briefs",
        ] {
            let result = server.call(method, json!({})).unwrap();
            assert_eq!(result.is_error, Some(true), "{method}");
            let ContentBlock::Text(text) = &result.content[0] else {
                panic!("expected text error");
            };
            assert!(
                text.text.contains("unknown method"),
                "{method}: {}",
                text.text
            );
            assert!(
                text.text.contains(
                    "removed in the attention-map redesign; use durable walkthrough steps \
                     and attention regions instead (walkthrough_*, attention_*)"
                ),
                "{method}: {}",
                text.text
            );
        }
    }

    #[test]
    fn ordering_and_flag_tools_persist_the_reduced_shared_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        server
            .set_ordering(Parameters(SetOrderingParams {
                paths: vec!["src/app.rs".into()],
            }))
            .unwrap();
        server
            .flag_section(Parameters(FlagSectionParams {
                path: "src/app.rs".into(),
                line: Some(1),
                reason: "risky".into(),
                priority: Some("critical".into()),
            }))
            .unwrap();

        let overlay =
            crate::agent::AgentOverlay::load_or_default(&dir.path().join("agent.json")).unwrap();
        assert_eq!(overlay.ordering, ["src/app.rs"]);
        assert_eq!(overlay.flags.len(), 1);
        assert_eq!(
            overlay.flags[0].priority,
            crate::agent::FlagPriority::Critical
        );
    }

    #[test]
    fn list_reviews_reads_the_registry() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());

        let result = result_json(&server.list_reviews().unwrap());
        assert_eq!(result, json!([]));
    }

    #[test]
    fn reviews_create_then_reviews_list_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());

        let created = result_json(
            &server
                .reviews_create(Parameters(ReviewsCreateParams {
                    title: Some("MCP pass".to_owned()),
                }))
                .unwrap(),
        );
        let listed = result_json(&server.reviews_list().unwrap());

        assert_eq!(created["title"], "MCP pass");
        assert_eq!(listed["sessions"][0]["id"], created["id"]);
    }

    #[test]
    fn mcp_disposition_set_show_clear_persists_every_transition() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        let created = result_json(
            &server
                .reviews_create(Parameters(ReviewsCreateParams { title: None }))
                .unwrap(),
        );
        let updated = result_json(
            &server
                .review_disposition_set(Parameters(ReviewDispositionParams {
                    disposition: Some(ReviewDisposition::RequestChanges),
                }))
                .unwrap(),
        );
        assert_eq!(updated["disposition"], "request-changes");
        assert_eq!(
            ReviewState::load_or_default(&server.state_path)
                .unwrap()
                .sessions[0]
                .disposition,
            Some(ReviewDisposition::RequestChanges)
        );
        let shown = result_json(&server.review_disposition().unwrap());
        assert_eq!(shown, json!({ "disposition": "request-changes" }));
        let cleared = result_json(
            &server
                .review_disposition_set(Parameters(ReviewDispositionParams { disposition: None }))
                .unwrap(),
        );
        assert_eq!(cleared["id"], created["id"]);
        assert!(cleared["disposition"].is_null());
        assert_eq!(
            ReviewState::load_or_default(&server.state_path)
                .unwrap()
                .sessions[0]
                .disposition,
            None
        );
        assert_eq!(
            result_json(&server.review_disposition().unwrap()),
            json!({ "disposition": null })
        );
    }

    #[test]
    fn comment_add_persists_to_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());

        let comment = result_json(
            &server
                .comment_add(Parameters(CommentAddParams {
                    path: Some("src/app.rs".to_owned()),
                    general: None,
                    line: Some(1),
                    end_line: None,
                    body: "persist me".to_owned(),
                    kind: Some(CommentKind::Issue),
                    action: Some(ActionIntent::Fix),
                    state: Some(InitialCommentState::Draft),
                    channel: None,
                }))
                .unwrap(),
        );

        let state = ReviewState::load_or_default(&server.state_path).unwrap();
        assert_eq!(state.comments.len(), 1);
        assert_eq!(state.comments[0].id, comment["id"]);
        assert_eq!(state.comments[0].body, "persist me");
        let observation = state.comments[0]
            .observation
            .as_ref()
            .expect("MCP captures its loaded diff");
        assert_eq!(observation.snapshot.files[0].path, "src/app.rs");
        assert!(matches!(
            observation.anchor.as_ref(),
            Some(crate::anchor::CommentAnchor::Line { line: 1, .. })
        ));
        assert_eq!(
            state.comments[0].anchor.as_ref(),
            observation.anchor.as_ref()
        );
    }

    #[test]
    fn mcp_comment_and_reply_use_configured_agent_identity_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let server = GanderMcp::new(
            move || session(&root),
            None,
            GanderMcpParams {
                overlay_path: dir.path().join("agent.json"),
                state_path: dir.path().join("state.json"),
                registry_dir: dir.path().join("registry"),
                workspace_root: dir.path().to_path_buf(),
                target: ReviewTarget::trunk_to_current(),
                diff_files: vec!["src/app.rs".into()],
                attention_files: attention_files(),
                generated_policy: GeneratedPolicy::default(),
                ignore_globs: Vec::new(),
                initial_comment_state: CommentState::Todo,
                agent_identity: Identity {
                    kind: crate::state::AuthorKind::Agent,
                    name: "MCP Bot".into(),
                },
            },
        )
        .unwrap();
        let comment = result_json(
            &server
                .comment_add(Parameters(CommentAddParams {
                    path: Some("src/app.rs".into()),
                    general: None,
                    line: Some(1),
                    end_line: None,
                    body: "configured".into(),
                    kind: None,
                    action: None,
                    state: None,
                    channel: None,
                }))
                .unwrap(),
        );
        assert_eq!(
            comment["author"],
            json!({ "kind": "agent", "name": "MCP Bot" })
        );
        let replied = result_json(
            &server
                .comment_reply(Parameters(CommentReplyParams {
                    id: comment["id"].as_str().unwrap().into(),
                    body: "configured reply".into(),
                    resolve: None,
                }))
                .unwrap(),
        );
        assert_eq!(
            replied["replies"][0]["author"],
            json!({ "kind": "agent", "name": "MCP Bot" })
        );
        let persisted = ReviewState::load_or_default(&server.state_path).unwrap();
        assert_eq!(persisted.comments[0].author.name, "MCP Bot");
        assert_eq!(persisted.comments[0].replies[0].author.name, "MCP Bot");
        assert_eq!(
            persisted.comments[0].replies[0].author.kind,
            crate::state::AuthorKind::Agent
        );
    }

    #[test]
    fn comment_add_freezes_range_anchor_from_selected_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let server = GanderMcp::new(
            move || {
                ReviewSession::new(
                    root,
                    ReviewTarget::trunk_to_current(),
                    DiffSet::parse(
                        "diff --git a/src/app.rs b/src/app.rs\n--- a/src/app.rs\n+++ b/src/app.rs\n@@ -1,2 +1,2 @@\n-old one\n-old two\n+new one\n+new two",
                    )
                    .unwrap(),
                    ReviewState::default(),
                )
            },
            None,
            GanderMcpParams {
                overlay_path: dir.path().join("agent.json"),
                state_path: dir.path().join("state.json"),
                registry_dir: dir.path().join("registry"),
                workspace_root: dir.path().to_path_buf(),
                target: ReviewTarget::trunk_to_current(),
                diff_files: vec!["src/app.rs".into()],
                attention_files: attention_files(),
                generated_policy: GeneratedPolicy::default(),
                ignore_globs: Vec::new(),
                initial_comment_state: CommentState::Todo,
                agent_identity: Identity::agent(),
            },
        )
        .unwrap();

        server
            .comment_add(Parameters(CommentAddParams {
                path: Some("src/app.rs".into()),
                general: None,
                line: Some(1),
                end_line: Some(2),
                body: "range".into(),
                kind: None,
                action: None,
                state: None,
                channel: None,
            }))
            .unwrap();
        server
            .comment_add(Parameters(CommentAddParams {
                path: Some("src/app.rs".into()),
                general: None,
                line: None,
                end_line: None,
                body: "file".into(),
                kind: None,
                action: None,
                state: None,
                channel: None,
            }))
            .unwrap();

        let state = ReviewState::load_or_default(&server.state_path).unwrap();
        let observation = state.comments[0].observation.as_ref().unwrap();
        assert!(matches!(
            observation.anchor.as_ref(),
            Some(crate::anchor::CommentAnchor::Range {
                start_line: 1,
                end_line: 2,
                ..
            })
        ));
        assert_eq!(
            state.comments[0].anchor.as_ref(),
            observation.anchor.as_ref()
        );
        assert!(matches!(
            state.comments[1]
                .observation
                .as_ref()
                .and_then(|observation| observation.anchor.as_ref()),
            Some(crate::anchor::CommentAnchor::File { .. })
        ));
    }

    #[test]
    fn reply_and_resolve_capture_results_but_plain_resolve_does_not_add_reply() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        let add = |body: &str| {
            result_json(
                &server
                    .comment_add(Parameters(CommentAddParams {
                        path: Some("src/app.rs".into()),
                        general: None,
                        line: Some(1),
                        end_line: None,
                        body: body.into(),
                        kind: None,
                        action: None,
                        state: None,
                        channel: None,
                    }))
                    .unwrap(),
            )
        };
        let plain = add("plain");
        let resolved = result_json(
            &server
                .comment_resolve(Parameters(CommentResolveParams {
                    id: plain["id"].as_str().unwrap().into(),
                    reply: None,
                }))
                .unwrap(),
        );
        assert_eq!(resolved["state"], "resolved");
        assert!(resolved.get("replies").is_none_or(Value::is_null));

        let replied = add("reply");
        let replied = result_json(
            &server
                .comment_reply(Parameters(CommentReplyParams {
                    id: replied["id"].as_str().unwrap().into(),
                    body: "fixed".into(),
                    resolve: Some(true),
                }))
                .unwrap(),
        );
        assert_eq!(replied["state"], "resolved");
        assert_eq!(
            replied["replies"][0]["result"]["parent_comment_id"],
            replied["id"]
        );
        assert!(replied["replies"][0]["result"]["observation_aggregate_fingerprint"].is_string());
        assert_eq!(
            replied["replies"][0]["result"]["portable_patch_changed"],
            false
        );
    }

    #[test]
    fn general_add_explicit_state_and_bulk_ready_share_core_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());

        let comment = result_json(
            &server
                .comment_add(Parameters(CommentAddParams {
                    path: None,
                    general: Some(true),
                    line: None,
                    end_line: None,
                    body: "session-wide feedback".to_owned(),
                    kind: Some(CommentKind::Question),
                    action: Some(ActionIntent::None),
                    state: Some(InitialCommentState::Draft),
                    channel: None,
                }))
                .unwrap(),
        );
        assert!(comment["path"].is_null());
        assert_eq!(comment["state"], "draft");
        assert!(comment["session_id"].as_str().is_some());

        // MCP additions are agent-authored, so the bulk sweep leaves them
        // as drafts awaiting human triage and reports the skip.
        let ready = result_json(
            &server
                .comments_ready(Parameters(CommentsReadyParams {
                    ids: None,
                    all_drafts: Some(true),
                }))
                .unwrap(),
        );
        assert_eq!(ready["readied"], 0);
        assert_eq!(ready["skipped_agent_drafts"], 1);
        let state = ReviewState::load_or_default(&server.state_path).unwrap();
        assert_eq!(state.comments[0].state, CommentState::Draft);

        // Explicit id selection still escalates the agent draft.
        let ready = result_json(
            &server
                .comments_ready(Parameters(CommentsReadyParams {
                    ids: Some(vec![comment["id"].as_str().unwrap().to_owned()]),
                    all_drafts: None,
                }))
                .unwrap(),
        );
        assert_eq!(ready["readied"], 1);
        let state = ReviewState::load_or_default(&server.state_path).unwrap();
        assert_eq!(state.comments[0].state, CommentState::Todo);
        assert!(state.comments[0].is_general());
    }

    #[test]
    fn comment_add_channel_param_matches_cli_semantics_including_todo_coercion() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());

        // Non-actionable channel demotes the configured todo initial state.
        let onboarding = result_json(
            &server
                .comment_add(Parameters(CommentAddParams {
                    path: Some("src/app.rs".into()),
                    general: None,
                    line: Some(1),
                    end_line: None,
                    body: "look here first".to_owned(),
                    kind: None,
                    action: None,
                    state: None,
                    channel: Some(Channel::Onboarding),
                }))
                .unwrap(),
        );
        assert_eq!(onboarding["channel"], "onboarding");
        assert_eq!(onboarding["state"], "draft");

        // A todo-permitting channel keeps the explicit todo state.
        let collaboration = result_json(
            &server
                .comment_add(Parameters(CommentAddParams {
                    path: None,
                    general: Some(true),
                    line: None,
                    end_line: None,
                    body: "team feedback".to_owned(),
                    kind: None,
                    action: None,
                    state: Some(InitialCommentState::Todo),
                    channel: Some(Channel::Collaboration),
                }))
                .unwrap(),
        );
        assert_eq!(collaboration["channel"], "collaboration");
        assert_eq!(collaboration["state"], "todo");

        // Omitted channel keeps the state-derived default.
        let derived = result_json(
            &server
                .comment_add(Parameters(CommentAddParams {
                    path: None,
                    general: Some(true),
                    line: None,
                    end_line: None,
                    body: "derived default".to_owned(),
                    kind: None,
                    action: None,
                    state: Some(InitialCommentState::Draft),
                    channel: None,
                }))
                .unwrap(),
        );
        assert_eq!(derived["channel"], "note");
        assert_eq!(derived["state"], "draft");

        let state = ReviewState::load_or_default(&server.state_path).unwrap();
        assert_eq!(state.comments.len(), 3);
        assert_eq!(state.comments[0].channel, Channel::Onboarding);
        assert_eq!(state.comments[0].state, CommentState::Draft);
        assert_eq!(state.comments[1].channel, Channel::Collaboration);
        assert_eq!(state.comments[1].state, CommentState::Todo);
    }

    #[test]
    fn action_item_close_sets_disposition_and_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        let item = result_json(
            &server
                .action_item_add(Parameters(ActionItemAddParams {
                    title: "Fix it".to_owned(),
                    body: None,
                    action: Some(ActionIntent::Fix),
                    comments: None,
                    path: None,
                    line: None,
                    end_line: None,
                    symbol: None,
                }))
                .unwrap(),
        );

        let closed = result_json(
            &server
                .action_item_close(Parameters(ActionItemCloseParams {
                    id: item["id"].as_str().unwrap().to_owned(),
                    disposition: McpClosedDisposition::Completed,
                    outcome: Some("done".to_owned()),
                }))
                .unwrap(),
        );

        assert_eq!(closed["status"], "closed");
        assert_eq!(closed["disposition"], "completed");
        assert_eq!(closed["outcome"], "done");

        let listed = result_json(&server.action_items_list().unwrap());
        assert_eq!(listed["action_items"][0]["id"], item["id"]);
    }

    #[test]
    fn walkthrough_add_step_then_show() {
        let dir = tempfile::tempdir().unwrap();
        let mut server = server(dir.path());
        server.agent_identity = Identity {
            kind: crate::state::AuthorKind::Agent,
            name: "Configured Walkthrough Agent".into(),
        };

        let step = result_json(
            &server
                .walkthrough_add_step(Parameters(WalkthroughAddStepParams {
                    title: "Read app".to_owned(),
                    file: Some("src/app.rs".to_owned()),
                    line: Some(1),
                    end_line: None,
                    symbol: None,
                    why: Some("entry point".to_owned()),
                    body: None,
                }))
                .unwrap(),
        );
        let shown = result_json(&server.walkthrough_show().unwrap());

        assert_eq!(shown["walkthroughs"][0]["steps"][0]["id"], step["id"]);
        assert_eq!(shown["walkthroughs"][0]["steps"][0]["title"], "Read app");
        assert_eq!(step["author"]["kind"], "agent");
        assert_eq!(step["author"]["name"], "Configured Walkthrough Agent");
        let state = ReviewState::load_or_default(&server.state_path).unwrap();
        assert_eq!(state.sessions[0].attention_regions.len(), 1);
        assert_eq!(
            state.sessions[0].attention_regions[0].source,
            crate::state::SalienceSource::Agent
        );
        assert!(
            state.sessions[0].attention_regions[0]
                .target
                .anchor
                .is_some()
        );
    }

    #[test]
    fn walkthrough_set_replaces_durable_curation_and_syncs_attention() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        let spec = WalkthroughSetParams {
            title: Some("Review path".into()),
            steps: vec![WalkthroughSetStepParams {
                id: None,
                kind: Some("step".into()),
                importance: Some("spotlight".into()),
                change_id: None,
                title: Some("Core invariant".into()),
                body: Some("Follow the new value through the entry point.".into()),
                why: Some("This is the mental-model delta.".into()),
                target: Some(WalkthroughTargetParams {
                    path: Some("src/app.rs".into()),
                    line: Some(1),
                    end_line: None,
                    symbol: None,
                }),
                extra_targets: Vec::new(),
                artifacts: vec![WalkthroughArtifactParams {
                    title: "flow".into(),
                    kind: Some("diagram".into()),
                    body: "input -> app".into(),
                }],
            }],
        };
        let result = result_json(&server.walkthrough_set(Parameters(spec.clone())).unwrap());
        let first_id = result["walkthrough"]["steps"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(result["walkthrough"]["title"], "Review path");
        assert_eq!(
            result["walkthrough"]["steps"][0]["artifacts"][0]["kind"],
            "diagram"
        );
        let repeated = result_json(&server.walkthrough_set(Parameters(spec)).unwrap());
        assert_eq!(
            repeated["walkthrough"]["steps"][0]["id"], first_id,
            "repeated omitted-id replacements must be idempotent"
        );
        let state = ReviewState::load_or_default(&server.state_path).unwrap();
        assert_eq!(state.sessions[0].attention_regions.len(), 1);
        assert_eq!(
            state.sessions[0].attention_regions[0].salience,
            Salience::Spotlight
        );
    }

    #[test]
    fn walkthrough_set_duplicate_id_error_matches_cli_and_does_not_mutate_state() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        let step = |id: &str, title: &str| WalkthroughSetStepParams {
            id: Some(id.into()),
            kind: Some("step".into()),
            importance: Some("spotlight".into()),
            change_id: None,
            title: Some(title.into()),
            body: None,
            why: None,
            target: Some(WalkthroughTargetParams {
                path: Some("src/app.rs".into()),
                line: Some(1),
                end_line: None,
                symbol: None,
            }),
            extra_targets: Vec::new(),
            artifacts: Vec::new(),
        };
        server
            .walkthrough_set(Parameters(WalkthroughSetParams {
                title: Some("Prior".into()),
                steps: vec![step("prior", "Prior")],
            }))
            .unwrap();
        let before = std::fs::read(&server.state_path).unwrap();

        let error = server
            .walkthrough_set(Parameters(WalkthroughSetParams {
                title: Some("Rejected".into()),
                steps: vec![step("duplicate", "First"), step("duplicate", "Second")],
            }))
            .unwrap_err();

        assert_eq!(
            error.message,
            "duplicate explicit walkthrough step id(s): duplicate"
        );
        assert_eq!(std::fs::read(&server.state_path).unwrap(), before);
    }

    #[test]
    fn walkthrough_set_validates_chapter_ids_against_current_stack_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let server = server_with_stack(dir.path());
        let chapter = |change_id: &str| WalkthroughSetParams {
            title: Some("Stack".into()),
            steps: vec![WalkthroughSetStepParams {
                id: None,
                kind: Some("chapter".into()),
                importance: None,
                change_id: Some(change_id.into()),
                title: Some("Chapter".into()),
                body: Some("Narrative".into()),
                why: None,
                target: None,
                extra_targets: Vec::new(),
                artifacts: Vec::new(),
            }],
        };

        let error = server
            .walkthrough_set(Parameters(chapter("abc")))
            .unwrap_err();
        assert!(error.message.contains("unknown change id"), "{error:?}");

        let result = result_json(
            &server
                .walkthrough_set(Parameters(chapter("abcdef")))
                .unwrap(),
        );
        assert_eq!(result["walkthrough"]["steps"][0]["change_id"], "abcdef");
    }

    #[test]
    fn attention_tools_share_set_promote_list_and_clear_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        let set = result_json(
            &server
                .attention_set(Parameters(AttentionMutationParams {
                    path: "src/app.rs".into(),
                    line: Some(1),
                    end_line: None,
                    salience: Salience::Skim,
                    rationale: Some("routine".into()),
                }))
                .unwrap(),
        );
        assert_eq!(set["source"], "human");
        assert_eq!(set["salience"], "skim");
        assert_eq!(set["stale"], false);

        let promoted = result_json(
            &server
                .attention_promote(Parameters(AttentionTargetParams {
                    path: "src/app.rs".into(),
                    line: Some(1),
                    end_line: None,
                    rationale: None,
                }))
                .unwrap(),
        );
        assert_eq!(promoted["salience"], "supporting");
        assert_eq!(promoted["stale"], false);
        let listed = result_json(
            &server
                .attention_list(Parameters(AttentionListParams {
                    assigned: Some(true),
                }))
                .unwrap(),
        );
        assert_eq!(listed["regions"].as_array().unwrap().len(), 1);
        assert_eq!(listed["regions"][0]["stale"], false);

        let cleared = result_json(
            &server
                .attention_clear(Parameters(AttentionClearParams {
                    path: "src/app.rs".into(),
                    line: Some(1),
                    end_line: None,
                }))
                .unwrap(),
        );
        assert_eq!(cleared["cleared"], true);
    }

    #[test]
    fn attention_coverage_and_skim_acknowledgement_match_cli_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        server
            .attention_set(Parameters(AttentionMutationParams {
                path: "src/app.rs".into(),
                line: None,
                end_line: None,
                salience: Salience::Skim,
                rationale: Some("generated churn".into()),
            }))
            .unwrap();
        let listed = result_json(&server.attention_skim_fold_list().unwrap());
        assert_eq!(listed["current"], 1);
        assert_eq!(listed["folds"][0]["acknowledged"], false);

        let outcome = result_json(
            &server
                .attention_skim_fold_acknowledge(Parameters(AttentionAcknowledgeParams {
                    path: None,
                    line: None,
                    end_line: None,
                    fold_id: None,
                    all: Some(true),
                }))
                .unwrap(),
        );
        assert_eq!(outcome["acknowledged"], 1);
        assert_eq!(outcome["whole_files_viewed"][0], "src/app.rs");
        let coverage = result_json(&server.attention_coverage_show().unwrap());
        assert_eq!(coverage["covered"], coverage["total"]);
        let state = ReviewState::load_or_default(&server.state_path).unwrap();
        assert!(state.files["src/app.rs"].viewed);
    }

    #[test]
    fn attention_acknowledge_rejects_mcp_selector_conflicts_and_invalid_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        let invalid = [
            (
                AttentionAcknowledgeParams {
                    path: Some("src/app.rs".into()),
                    line: None,
                    end_line: None,
                    fold_id: Some("fold:id".into()),
                    all: None,
                },
                "fold_id cannot be combined",
            ),
            (
                AttentionAcknowledgeParams {
                    path: None,
                    line: Some(1),
                    end_line: None,
                    fold_id: Some("fold:id".into()),
                    all: None,
                },
                "fold_id cannot be combined",
            ),
            (
                AttentionAcknowledgeParams {
                    path: Some("src/app.rs".into()),
                    line: Some(1),
                    end_line: None,
                    fold_id: None,
                    all: Some(true),
                },
                "all=true cannot be combined",
            ),
            (
                AttentionAcknowledgeParams {
                    path: None,
                    line: Some(1),
                    end_line: None,
                    fold_id: None,
                    all: None,
                },
                "line requires path",
            ),
            (
                AttentionAcknowledgeParams {
                    path: Some("src/app.rs".into()),
                    line: Some(0),
                    end_line: None,
                    fold_id: None,
                    all: None,
                },
                "1-indexed",
            ),
            (
                AttentionAcknowledgeParams {
                    path: Some("src/app.rs".into()),
                    line: Some(4),
                    end_line: Some(2),
                    fold_id: None,
                    all: None,
                },
                "greater than or equal",
            ),
        ];
        for (params, expected) in invalid {
            let error = server
                .attention_skim_fold_acknowledge(Parameters(params))
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
        }
        let unknown = server
            .attention_skim_fold_acknowledge(Parameters(AttentionAcknowledgeParams {
                path: None,
                line: None,
                end_line: None,
                fold_id: Some("fold:missing".into()),
                all: None,
            }))
            .unwrap_err()
            .to_string();
        assert!(unknown.contains("unknown skim fold id"), "{unknown}");
    }

    #[test]
    fn attention_clear_handles_missing_and_out_of_range_stale_identities() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        server
            .attention_set(Parameters(AttentionMutationParams {
                path: "src/app.rs".into(),
                line: None,
                end_line: None,
                salience: Salience::Skim,
                rationale: None,
            }))
            .unwrap();
        let mut state = ReviewState::load_or_default(&server.state_path).unwrap();
        let target = &mut state.sessions[0].attention_regions[0].target;
        target.file = Some("missing.rs".into());
        if let Some(crate::anchor::CommentAnchor::File { path, .. }) = &mut target.anchor {
            *path = "missing.rs".into();
        }
        state.save(&server.state_path).unwrap();
        let cleared = result_json(
            &server
                .attention_clear(Parameters(AttentionClearParams {
                    path: "missing.rs".into(),
                    line: None,
                    end_line: None,
                }))
                .unwrap(),
        );
        assert_eq!(cleared["cleared"], true);

        server
            .attention_set(Parameters(AttentionMutationParams {
                path: "src/app.rs".into(),
                line: Some(1),
                end_line: None,
                salience: Salience::Skim,
                rationale: None,
            }))
            .unwrap();
        let mut state = ReviewState::load_or_default(&server.state_path).unwrap();
        state.sessions[0].attention_regions[0].target.line = Some(99);
        state.save(&server.state_path).unwrap();
        let cleared = result_json(
            &server
                .attention_clear(Parameters(AttentionClearParams {
                    path: "src/app.rs".into(),
                    line: Some(99),
                    end_line: None,
                }))
                .unwrap(),
        );
        assert_eq!(cleared["cleared"], true);

        assert!(
            server
                .attention_set(Parameters(AttentionMutationParams {
                    path: "missing.rs".into(),
                    line: None,
                    end_line: None,
                    salience: Salience::Skim,
                    rationale: None,
                }))
                .is_err()
        );
        assert!(
            server
                .attention_promote(Parameters(AttentionTargetParams {
                    path: "src/app.rs".into(),
                    line: Some(99),
                    end_line: None,
                    rationale: None,
                }))
                .is_err()
        );
        assert!(
            server
                .attention_demote(Parameters(AttentionTargetParams {
                    path: "missing.rs".into(),
                    line: None,
                    end_line: None,
                    rationale: None,
                }))
                .is_err()
        );
    }

    #[test]
    fn attention_heuristics_honor_mcp_custom_generated_and_ignore_config() {
        let dir = tempfile::tempdir().unwrap();
        let mut server = server(dir.path());
        server.attention_files = DiffSet::parse(
            "diff --git a/gen/client.ts b/gen/client.ts\n--- a/gen/client.ts\n+++ b/gen/client.ts\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/ignored/snapshot.txt b/ignored/snapshot.txt\n--- a/ignored/snapshot.txt\n+++ b/ignored/snapshot.txt\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap()
        .files;
        server.diff_files.clear();
        server.generated_policy = GeneratedPolicy {
            presets: Vec::new(),
            globs: vec!["gen/**".into()],
        };
        server.ignore_globs = vec!["ignored/**".into()];

        let seeded = result_json(&server.attention_seed_heuristics().unwrap());
        let regions = seeded["regions"].as_array().unwrap();
        assert_eq!(regions.len(), 2);
        assert!(regions.iter().any(|region| {
            region["target"]["file"] == "gen/client.ts"
                && region["rationale"]
                    .as_str()
                    .unwrap()
                    .contains("generated path policy")
        }));
        assert!(regions.iter().any(|region| {
            region["target"]["file"] == "ignored/snapshot.txt"
                && region["rationale"]
                    .as_str()
                    .unwrap()
                    .contains("ignore policy")
        }));
        assert!(regions.iter().all(|region| region["stale"] == false));
    }

    #[cfg(unix)]
    #[test]
    fn tools_route_to_a_live_instance_when_one_serves_the_workspace() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().unwrap();
        let socket_path = dir.path().join("acp-9.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        // A stand-in for a live TUI: the registry liveness probe connects
        // and hangs up without sending anything, so accept until a
        // connection actually carries a request, then answer it.
        let responder = std::thread::spawn(move || {
            loop {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    continue; // liveness probe
                }
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], "review/summary");
                let mut stream = stream;
                writeln!(
                    stream,
                    r#"{{"jsonrpc":"2.0","id":1,"result":{{"summary":"live"}}}}"#
                )
                .unwrap();
                break;
            }
        });
        let registry_dir = dir.path().join("registry");
        let _registration = registry::InstanceRegistration::register(
            &registry_dir,
            registry::InstanceInfo {
                pid: 9,
                workspace_root: workspace.clone(),
                base: "trunk()".to_owned(),
                rev: "@".to_owned(),
                summary: "1 file".to_owned(),
                socket_path,
                started_at: chrono::Utc::now(),
                last_input_at: chrono::Utc::now(),
            },
        )
        .unwrap();
        let root = workspace.clone();
        let server = GanderMcp::new(
            move || session(&root),
            None,
            GanderMcpParams {
                overlay_path: dir.path().join("agent.json"),
                state_path: dir.path().join("state.json"),
                registry_dir,
                workspace_root: workspace,
                target: ReviewTarget::trunk_to_current(),
                diff_files: vec!["src/app.rs".into()],
                attention_files: attention_files(),
                generated_policy: GeneratedPolicy::default(),
                ignore_globs: Vec::new(),
                initial_comment_state: CommentState::Todo,
                agent_identity: Identity::agent(),
            },
        )
        .unwrap();

        let summary = result_json(&server.review_summary().unwrap());

        assert_eq!(summary["summary"], "live");
        responder.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn comment_capture_uses_selected_live_context_not_stale_startup_context() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().unwrap();
        let socket_path = dir.path().join("acp-live-context.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let live_diff = DiffSet::parse(
            "diff --git a/live.rs b/live.rs\n--- a/live.rs\n+++ b/live.rs\n@@ -7 +7 @@\n-old\n+live",
        )
        .unwrap();
        let response_repo = workspace.clone();
        let response_files = live_diff.files.clone();
        let responder = std::thread::spawn(move || {
            loop {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    continue;
                }
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], "review/provenance_context");
                let mut stream = stream;
                writeln!(
                    stream,
                    "{}",
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {
                            "repo": response_repo,
                            "base": "live-base",
                            "revision": "live-rev",
                            "files": response_files,
                        }
                    })
                )
                .unwrap();
                break;
            }
        });
        let registry_dir = dir.path().join("registry");
        let _registration = registry::InstanceRegistration::register(
            &registry_dir,
            registry::InstanceInfo {
                pid: 10,
                workspace_root: workspace.clone(),
                base: "live-base".into(),
                rev: "live-rev".into(),
                summary: "live target".into(),
                socket_path,
                started_at: chrono::Utc::now(),
                last_input_at: chrono::Utc::now(),
            },
        )
        .unwrap();
        let startup_root = workspace.clone();
        let server = GanderMcp::new(
            move || session(&startup_root),
            None,
            GanderMcpParams {
                overlay_path: dir.path().join("agent.json"),
                state_path: dir.path().join("state.json"),
                registry_dir,
                workspace_root: workspace.clone(),
                target: ReviewTarget::trunk_to_current(),
                diff_files: vec!["src/app.rs".into()],
                attention_files: attention_files(),
                generated_policy: GeneratedPolicy::default(),
                ignore_globs: Vec::new(),
                initial_comment_state: CommentState::Todo,
                agent_identity: Identity::agent(),
            },
        )
        .unwrap();

        server
            .comment_add(Parameters(CommentAddParams {
                path: Some("live.rs".into()),
                general: None,
                line: Some(7),
                end_line: None,
                body: "live observation".into(),
                kind: None,
                action: None,
                state: None,
                channel: None,
            }))
            .unwrap();
        responder.join().unwrap();

        let state = ReviewState::load_or_default(&server.state_path).unwrap();
        let observation = state.comments[0].observation.as_ref().unwrap();
        assert_eq!(
            observation.snapshot.identity.target.base.as_deref(),
            Some("live-base")
        );
        assert_eq!(
            observation.snapshot.identity.target.revision.as_deref(),
            Some("live-rev")
        );
        assert_eq!(observation.snapshot.files[0].path, "live.rs");
        assert!(matches!(
            observation.anchor.as_ref(),
            Some(crate::anchor::CommentAnchor::Line { line: 7, .. })
        ));
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].target.base.as_deref(), Some("live-base"));
        assert_eq!(
            state.sessions[0].target.repo.as_deref(),
            Some(workspace.to_str().unwrap())
        );
    }
}
