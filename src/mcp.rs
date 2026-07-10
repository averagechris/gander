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
    jj::{JjBackend, ReviewTarget as JjReviewTarget},
    registry, review,
    state::{
        ActionIntent, CommentKind, CommentState, ReviewState, ReviewTarget as StateReviewTarget,
        WalkthroughStep,
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
    initial_comment_state: CommentState,
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
    pub initial_comment_state: CommentState,
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

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ChunkPartParams {
    pub path: String,
    pub start_line: Option<u64>,
    pub end_line: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ArtifactParams {
    /// Short title for the exhibit.
    pub title: String,
    /// example | output | diagram | note (default example).
    pub kind: Option<String>,
    /// Plain text body (code, captured output, ASCII diagram, prose),
    /// rendered verbatim in a scrollable viewer.
    pub body: String,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ChunkParams {
    /// Stable chunk id. Omit to generate a new id.
    pub id: Option<String>,
    /// Short human-readable title for the reviewable unit.
    pub title: String,
    /// spotlight for the few stops worth touring; glance for routine/context
    /// hunks that should stay in the at-a-glance rail instead of interrupting
    /// the walkthrough.
    pub importance: Option<String>,
    /// The jj change this chunk belongs to (a change_id from
    /// `stack_changes`). The zen walkthrough retargets the review to that
    /// change's own diff for this stop, so part line numbers must come from
    /// `change_diff` for the same change. Anchor chunks this way whenever the
    /// review target spans a stack of changes; omit for chunks over the
    /// loaded target as a whole.
    pub change_id: Option<String>,
    /// Why these parts belong together.
    pub rationale: Option<String>,
    /// For spotlight chunks: 2-5 sentences that teach the change (what the
    /// code does, why it changed, what could break). Shown full-screen on
    /// the zen focus card.
    pub explanation: Option<String>,
    /// Optional exhibits that show the change rather than describe it — a
    /// usage example, output captured by exercising the code, a small
    /// diagram. The human opens them from the stop's card with `e`.
    pub artifacts: Option<Vec<ArtifactParams>>,
    pub parts: Vec<ChunkPartParams>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetChunksParams {
    /// Reviewable units that can span or subdivide files; replaces the
    /// previous set.
    pub chunks: Vec<ChunkParams>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UpdateChunksParams {
    /// Reviewable units to upsert: matching ids replace in place, new ids append.
    pub chunks: Vec<ChunkParams>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RemoveChunksParams {
    /// Chunk ids to remove. Unknown ids are rejected without applying changes.
    pub ids: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ChangeBriefParams {
    /// The jj change this brief describes (a change_id from `stack_changes`).
    pub change_id: String,
    /// 2-4 sentences of prose that teach the change at a high level: what it
    /// accomplishes, why it exists, and how it builds on the changes before
    /// it. Shown on the chapter intro card before that change's walkthrough
    /// stops.
    pub summary: String,
    /// Optional exhibits for the chapter card: examples, captured output,
    /// diagrams. The human opens them with `e`.
    pub artifacts: Option<Vec<ArtifactParams>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetChangeBriefsParams {
    /// One brief per change in the reviewed range; replaces the previous set.
    pub briefs: Vec<ChangeBriefParams>,
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
            let session = session_factory();
            while let Ok(request) = receiver.recv() {
                // Pick up dispositions the TUI may have written to the
                // shared overlay since the last call.
                handler.refresh_overlay();
                let response = handler.handle_line(&session, &request.line);
                let _ = request.reply.send(response);
            }
        });
        Ok(Self {
            registry_dir: params.registry_dir,
            workspace_root,
            state_path: params.state_path,
            target: target_spec,
            diff_files: params.diff_files,
            initial_comment_state: params.initial_comment_state,
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
    fn comments(&self) -> Result<CallToolResult, McpError> {
        self.call("review/comments", Value::Null)
    }

    #[tool(
        description = "What the human reviewer is looking at right now: focused pane, selected file, and line/hunk when available"
    )]
    fn current_focus(&self) -> Result<CallToolResult, McpError> {
        self.call("review/current_focus", Value::Null)
    }

    #[tool(
        description = "Live TUI presentation status: active tour, slide index/count, phase, and current slide target"
    )]
    fn present_status(&self) -> Result<CallToolResult, McpError> {
        self.call("present/status", Value::Null)
    }

    #[tool(description = "Start the live TUI tour, like pressing T")]
    fn present_start(&self) -> Result<CallToolResult, McpError> {
        self.call("present/start", Value::Null)
    }

    #[tool(description = "End the live TUI tour")]
    fn present_end(&self) -> Result<CallToolResult, McpError> {
        self.call("present/end", Value::Null)
    }

    #[tool(description = "Advance the live TUI tour to the next slide")]
    fn present_next(&self) -> Result<CallToolResult, McpError> {
        self.call("present/next", Value::Null)
    }

    #[tool(description = "Move the live TUI tour to the previous slide")]
    fn present_prev(&self) -> Result<CallToolResult, McpError> {
        self.call("present/prev", Value::Null)
    }

    #[tool(description = "Jump the live TUI tour to a zero-based slide index or durable step id")]
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

    #[tool(description = "Reload review/walkthrough state from disk and rebuild the live TUI tour")]
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
        description = "Git-style diff of one jj change against its parent (change_id- .. change_id). Line numbers in this diff are what chunk parts anchored to this change_id must reference"
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
        description = "Group the change into logical reviewable units that can span or subdivide files; anchor each unit to its jj change with change_id when reviewing a stack"
    )]
    fn set_chunks(
        &self,
        Parameters(params): Parameters<SetChunksParams>,
    ) -> Result<CallToolResult, McpError> {
        let chunks = serde_json::to_value(&params.chunks)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        self.call("review/set_chunks", json!({ "chunks": chunks }))
    }

    #[tool(
        description = "Incrementally upsert review chunks: matching ids replace in place; new ids append"
    )]
    fn update_chunks(
        &self,
        Parameters(params): Parameters<UpdateChunksParams>,
    ) -> Result<CallToolResult, McpError> {
        let chunks = serde_json::to_value(&params.chunks)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        self.call("review/update_chunks", json!({ "chunks": chunks }))
    }

    #[tool(
        description = "Remove review chunks by id; unknown ids are rejected without applying changes"
    )]
    fn remove_chunks(
        &self,
        Parameters(params): Parameters<RemoveChunksParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call("review/remove_chunks", json!({ "ids": params.ids }))
    }

    #[tool(
        description = "Brief the human on each jj change in the reviewed range: a short high-level narrative per change (what it accomplishes, why it exists, how it builds on the previous changes). The zen walkthrough shows each brief as a chapter intro card before that change's stops"
    )]
    fn set_change_briefs(
        &self,
        Parameters(params): Parameters<SetChangeBriefsParams>,
    ) -> Result<CallToolResult, McpError> {
        let briefs = serde_json::to_value(&params.briefs)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        self.call("review/set_change_briefs", json!({ "briefs": briefs }))
    }

    #[tool(
        description = "Draft a review comment for the human to accept, edit, or discard in the TUI"
    )]
    fn draft_comment(
        &self,
        Parameters(params): Parameters<DraftCommentParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call(
            "review/draft_comment",
            json!({
                "path": params.path,
                "line": params.line,
                "body": params.body,
            }),
        )
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
        description = "Add a durable review comment. Equivalent to `gander comments add`; external additions merge into a running TUI."
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
                    state: params
                        .state
                        .map(Into::into)
                        .unwrap_or(this.initial_comment_state),
                },
            )
        })
    }

    #[tool(
        description = "Mark selected durable comments, or all active-session drafts, ready as todos. Equivalent to `gander comments ready`."
    )]
    fn comments_ready(
        &self,
        Parameters(params): Parameters<CommentsReadyParams>,
    ) -> Result<CallToolResult, McpError> {
        let all_drafts = params.all_drafts.unwrap_or(false);
        let ids = params.ids.unwrap_or_default();
        if all_drafts != ids.is_empty() {
            return Err(McpError::invalid_params(
                "provide non-empty `ids` or `all_drafts: true`, but not both",
                None,
            ));
        }
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index(state);
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
        let context = params
            .reply
            .as_ref()
            .map(|_| self.selected_review_context())
            .transpose()?;
        self.with_state_mut(|state, this| {
            let idx = match &context {
                Some(context) => this.ensure_session_index_for_context(state, context),
                None => this.ensure_session_index(state),
            };
            if let Some(reply) = params.reply {
                let snapshot = Self::provenance_snapshot(
                    context.as_ref().expect("reply context captured"),
                    &state.sessions[idx],
                );
                review::reply_and_maybe_resolve_comment(
                    &mut state.sessions[idx],
                    &mut state.comments,
                    &params.id,
                    reply,
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
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index(state);
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
        description = "Add a durable walkthrough step. Equivalent to `gander walkthrough add-step`; external additions merge into a running TUI."
    )]
    fn walkthrough_add_step(
        &self,
        Parameters(params): Parameters<WalkthroughAddStepParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(file) = params.file.as_deref() {
            self.ensure_diff_file(file)?;
        }
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index(state);
            Ok(review::add_walkthrough_step(
                &mut state.sessions[idx],
                WalkthroughStep {
                    id: String::new(),
                    title: Some(params.title),
                    body: params.body,
                    why: params.why,
                    target: StateReviewTarget {
                        file: params.file,
                        line: params.line,
                        end_line: params.end_line,
                        symbol: params.symbol,
                        ..StateReviewTarget::default()
                    },
                    ..WalkthroughStep::default()
                },
            ))
        })
    }

    #[tool(
        description = "Remove a durable walkthrough step. Equivalent to `gander walkthrough remove-step <id>`."
    )]
    fn walkthrough_remove_step(
        &self,
        Parameters(params): Parameters<IdParams>,
    ) -> Result<CallToolResult, McpError> {
        self.with_state_mut(|state, this| {
            let idx = this.ensure_session_index(state);
            review::remove_walkthrough_step(&mut state.sessions[idx], &params.id)
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
                 flag_section, set_change_briefs (one short high-level brief \
                 per change in the stack — shown as a chapter intro before that \
                 change's walkthrough stops), set_chunks/update_chunks/remove_chunks (3-7 \
                 importance=spotlight chunks with a \
                 teaching `explanation` each; importance=glance for the routine \
                 rest — the human tours spotlights full-screen and skims glance \
                 items in bulk; on a stack, give each chunk the change_id it \
                 belongs to with line numbers from that change_diff, in stack \
                 order, so the walkthrough flows through the stack change by \
                 change; spotlight chunks and briefs may attach artifacts — \
                 examples, captured output, diagrams the human opens with `e`), \
                 and draft_comment — suggestions appear live in the \
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
                initial_comment_state: CommentState::Todo,
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
    fn write_tools_persist_the_shared_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());

        let draft = result_json(
            &server
                .draft_comment(Parameters(DraftCommentParams {
                    path: "src/app.rs".to_owned(),
                    line: Some(1),
                    body: "handle the None case".to_owned(),
                }))
                .unwrap(),
        );
        assert!(draft["id"].as_str().is_some());

        server
            .set_chunks(Parameters(SetChunksParams {
                chunks: vec![ChunkParams {
                    id: None,
                    title: "core change".to_owned(),
                    importance: Some("spotlight".to_owned()),
                    change_id: None,
                    rationale: None,
                    explanation: Some("Explains the core change.".to_owned()),
                    artifacts: None,
                    parts: vec![ChunkPartParams {
                        path: "src/app.rs".to_owned(),
                        start_line: Some(1),
                        end_line: Some(1),
                    }],
                }],
            }))
            .unwrap();

        server
            .set_change_briefs(Parameters(SetChangeBriefsParams {
                briefs: vec![ChangeBriefParams {
                    change_id: "abc".to_owned(),
                    summary: "Reworks the core loop before the follow-ups build on it.".to_owned(),
                    artifacts: None,
                }],
            }))
            .unwrap();

        let overlay =
            crate::agent::AgentOverlay::load_or_default(&dir.path().join("agent.json")).unwrap();
        assert_eq!(overlay.drafts.len(), 1);
        assert_eq!(overlay.chunks[0].title, "core change");
        assert_eq!(overlay.chunks[0].change_id, None);
        assert_eq!(
            overlay.chunks[0].explanation.as_deref(),
            Some("Explains the core change.")
        );
        assert_eq!(overlay.briefs.len(), 1);
        assert_eq!(overlay.briefs[0].change_id, "abc");
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
                initial_comment_state: CommentState::Todo,
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
                }))
                .unwrap(),
        );
        assert!(comment["path"].is_null());
        assert_eq!(comment["state"], "draft");
        assert!(comment["session_id"].as_str().is_some());

        let ready = result_json(
            &server
                .comments_ready(Parameters(CommentsReadyParams {
                    ids: None,
                    all_drafts: Some(true),
                }))
                .unwrap(),
        );
        assert_eq!(ready["readied"], 1);
        let state = ReviewState::load_or_default(&server.state_path).unwrap();
        assert_eq!(state.comments[0].state, CommentState::Todo);
        assert!(state.comments[0].is_general());
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
        let server = server(dir.path());

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
                initial_comment_state: CommentState::Todo,
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
                initial_comment_state: CommentState::Todo,
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
