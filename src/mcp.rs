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

use std::{
    path::{Path, PathBuf},
    sync::mpsc,
};

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

use crate::{acp::AcpHandler, app::ReviewSession, registry};

/// MCP server state: registry routing plus an in-process snapshot fallback.
///
/// [`ReviewSession`] is deliberately single-threaded (interior caches), so
/// the snapshot lives on its own dispatch thread and tool calls reach it
/// through a channel — the same shape as the TUI's live socket bridge.
pub struct GanderMcp {
    registry_dir: PathBuf,
    workspace_root: PathBuf,
    snapshot: mpsc::Sender<SnapshotRequest>,
    tool_router: ToolRouter<Self>,
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
pub struct ChunkParams {
    /// Short human-readable title for the reviewable unit.
    pub title: String,
    /// Why these parts belong together.
    pub rationale: Option<String>,
    pub parts: Vec<ChunkPartParams>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetChunksParams {
    /// Reviewable units that can span or subdivide files; replaces the
    /// previous set.
    pub chunks: Vec<ChunkParams>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DraftCommentParams {
    pub path: String,
    /// New-side line number the comment anchors to.
    pub line: Option<u64>,
    pub body: String,
}

#[tool_router]
impl GanderMcp {
    pub fn new(
        session_factory: impl FnOnce() -> ReviewSession + Send + 'static,
        overlay_path: PathBuf,
        registry_dir: PathBuf,
        workspace_root: PathBuf,
    ) -> Result<Self> {
        let mut handler = AcpHandler::new(overlay_path)?;
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
            registry_dir,
            workspace_root,
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
        description = "Group the change into logical reviewable units that can span or subdivide files"
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
        let response = match self.call_live(&request) {
            Some(response) => response?,
            None => self.call_snapshot(&request)?,
        };
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

    /// `None` when no live instance serves this workspace; the caller then
    /// falls back to the snapshot.
    #[cfg(unix)]
    fn call_live(&self, request: &str) -> Option<Result<Value, McpError>> {
        use std::io::{BufRead, BufReader, Write};

        let instance = registry::find_live_for_workspace(&self.registry_dir, &self.workspace_root)?;
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
                 see what the human is looking at with current_focus; then help \
                 organize the review with set_ordering, flag_section, set_chunks, \
                 and draft_comment — suggestions appear live in the reviewer's \
                 terminal, and drafted comments are triaged by the human. \
                 list_reviews shows every running review instance. Do not modify \
                 the repository.",
            )
    }
}

/// Serve MCP on stdio until the client disconnects. The factory builds the
/// snapshot fallback session on its owning thread.
pub fn run(
    session_factory: impl FnOnce() -> ReviewSession + Send + 'static,
    overlay_path: PathBuf,
    registry_dir: PathBuf,
    workspace_root: &Path,
) -> Result<()> {
    let server = GanderMcp::new(
        session_factory,
        overlay_path,
        registry_dir,
        workspace_root.to_path_buf(),
    )?;
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
            dir.join("agent.json"),
            dir.join("registry"),
            dir.to_path_buf(),
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
                    title: "core change".to_owned(),
                    rationale: None,
                    parts: vec![ChunkPartParams {
                        path: "src/app.rs".to_owned(),
                        start_line: Some(1),
                        end_line: Some(1),
                    }],
                }],
            }))
            .unwrap();

        let overlay =
            crate::agent::AgentOverlay::load_or_default(&dir.path().join("agent.json")).unwrap();
        assert_eq!(overlay.drafts.len(), 1);
        assert_eq!(overlay.chunks[0].title, "core change");
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
            dir.path().join("agent.json"),
            registry_dir,
            workspace,
        )
        .unwrap();

        let summary = result_json(&server.review_summary().unwrap());

        assert_eq!(summary["summary"], "live");
        responder.join().unwrap();
    }
}
