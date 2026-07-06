//! Agent-collaborative review over ACP.
//!
//! `gander acp` exposes the current review session over a line-delimited
//! JSON-RPC 2.0 protocol on stdio (the transport used by the Agent Client
//! Protocol). Agents can read the diff, comments, and viewed state, and can
//! write review suggestions (ordering, flagged sections, chunks, draft
//! comments) into the shared agent overlay that the TUI surfaces live.
//!
//! Two hosting modes share the same [`AcpHandler`] dispatch:
//! - standalone: [`AcpServer`] owns a session snapshot and serves stdio
//!   (used when no TUI is running);
//! - live: the TUI binds a Unix socket (see [`socket`]) and answers requests
//!   from its event loop against the live session, so agents see current
//!   viewed state, comments, and the active target. `gander acp` bridges
//!   stdio to that socket automatically when it exists.
//!
//! See docs/acp.md for the method reference.

use std::{
    io::{BufRead, Write},
    path::PathBuf,
};

use color_eyre::eyre::Result;
use serde_json::{Value, json};

use crate::{
    agent::{
        AgentDraft, AgentFlag, AgentOverlay, Artifact, ArtifactKind, ChangeBrief,
        ChangeDiffContext, ChunkImportance, ChunkPart, ChunkValidationContext, DraftState,
        FlagPriority, ReviewChunk, invalid_chunk_parts_message, remove_review_chunks,
        replace_review_chunks, update_review_chunks,
    },
    anchor::CommentAnchor,
    app::{Focus, ReviewSession},
    jj::{JjBackend, ReviewTarget},
};

pub const ACP_PROTOCOL_VERSION: u32 = 1;

/// Method dispatch plus overlay persistence, independent of transport and of
/// who owns the session (snapshot or live TUI session).
pub struct AcpHandler {
    overlay: AgentOverlay,
    overlay_path: PathBuf,
    /// Backend for stack/change queries (`review/stack_changes`,
    /// `review/change_diff`). Optional so tests and callers without jj
    /// access degrade to a clear per-method error.
    jj: Option<Box<dyn JjBackend + Send>>,
    live_session: bool,
}

/// Standalone stdio server owning a session snapshot.
pub struct AcpServer {
    session: ReviewSession,
    handler: AcpHandler,
}

impl AcpServer {
    pub fn new(session: ReviewSession, overlay_path: PathBuf) -> Result<Self> {
        Ok(Self {
            session,
            handler: AcpHandler::new(overlay_path)?,
        })
    }

    /// Enable the jj-backed stack methods (`review/stack_changes`,
    /// `review/change_diff`).
    pub fn with_jj(mut self, jj: Box<dyn JjBackend + Send>) -> Self {
        self.handler.set_jj_backend(jj);
        self
    }

    /// Serve requests until EOF. One JSON-RPC message per line.
    pub fn serve(&mut self, input: impl BufRead, mut output: impl Write) -> Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(response) = self.handler.handle_line(&self.session, &line) {
                serde_json::to_writer(&mut output, &response)?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn handle_line(&mut self, line: &str) -> Option<Value> {
        self.handler.handle_line(&self.session, line)
    }
}

impl AcpHandler {
    pub fn new(overlay_path: PathBuf) -> Result<Self> {
        let overlay = AgentOverlay::load_or_default(&overlay_path)?;
        Ok(Self {
            overlay,
            overlay_path,
            jj: None,
            live_session: false,
        })
    }

    pub fn mark_live_session(&mut self) {
        self.live_session = true;
    }

    /// Attach a jj backend so agents can inspect the stack and per-change
    /// diffs. Without one those methods return a descriptive error.
    pub fn set_jj_backend(&mut self, jj: Box<dyn JjBackend + Send>) {
        self.jj = Some(jj);
    }

    /// Re-read the overlay from disk so reads reflect dispositions another
    /// process (the TUI or a standalone server) wrote since our last look.
    pub fn refresh_overlay(&mut self) {
        if let Ok(overlay) = AgentOverlay::load_or_default(&self.overlay_path) {
            self.overlay = overlay;
        }
    }

    pub fn overlay(&self) -> &AgentOverlay {
        &self.overlay
    }

    /// Handle one raw JSON-RPC message; `None` means no response is due
    /// (notification).
    pub fn handle_line(&mut self, session: &ReviewSession, line: &str) -> Option<Value> {
        let request: Value = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(error) => {
                return Some(error_response(
                    Value::Null,
                    -32700,
                    &format!("parse error: {error}"),
                ));
            }
        };
        let id = request.get("id").cloned();
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        let result = self.dispatch(session, &method, &params);
        let id = id?;
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => error_response(id, -32000, &message),
        })
    }

    fn dispatch(
        &mut self,
        session: &ReviewSession,
        method: &str,
        params: &Value,
    ) -> Result<Value, String> {
        match method {
            "initialize" => Ok(json!({
                "protocol": "gander-acp",
                "version": ACP_PROTOCOL_VERSION,
                "capabilities": [
                    "review/summary",
                    "review/files",
                    "review/file_diff",
                    "review/comments",
                    "review/current_focus",
                    "review/overlay",
                    "review/stack_changes",
                    "review/change_diff",
                    "review/set_ordering",
                    "review/flag_section",
                    "review/set_chunks",
                    "review/update_chunks",
                    "review/remove_chunks",
                    "review/set_change_briefs",
                    "review/draft_comment",
                ],
            })),
            "review/summary" => Ok(json!({
                "repo": session.repo.display().to_string(),
                "base": session.target.base,
                "revision": session.target.rev,
                "active_target": session.target.to_string(),
                "live_session": self.live_session,
                "summary": session.summary_line(),
            })),
            "review/files" => Ok(json!(
                session
                    .files
                    .iter()
                    .map(|file| json!({
                        "path": file.path,
                        "old_path": file.old_path,
                        "status": file.status.to_string(),
                        "additions": file.additions,
                        "deletions": file.deletions,
                        "viewed": file.viewed,
                        "generated": file.generated,
                        "fingerprint": file.fingerprint,
                    }))
                    .collect::<Vec<_>>()
            )),
            "review/file_diff" => {
                let path = require_str(params, "path")?;
                let file = session
                    .files
                    .iter()
                    .find(|file| file.path == path)
                    .ok_or_else(|| format!("unknown file: {path}"))?;
                Ok(json!({
                    "path": file.path,
                    "fingerprint": file.fingerprint,
                    "raw": file.diff.raw,
                }))
            }
            "review/comments" => Ok(json!(
                session
                    .comments
                    .iter()
                    .map(|comment| json!({
                        "id": comment.id,
                        "path": comment.path,
                        "line": comment.line,
                        "end_line": comment.end_line,
                        "body": comment.body,
                        "state": comment.state.label(),
                    }))
                    .collect::<Vec<_>>()
            )),
            "review/overlay" => {
                serde_json::to_value(&self.overlay).map_err(|error| error.to_string())
            }
            // The jj stack the review lives in (`trunk()..@`, oldest first).
            // The human often treats these as stacked PRs, so agents should
            // organize chunks change-by-change when several changes exist.
            "review/stack_changes" => {
                let jj = self.require_jj()?;
                let stack = jj
                    .stack_changes(&session.repo)
                    .map_err(|error| format!("failed to load jj stack: {error}"))?;
                let current = stack
                    .iter()
                    .position(|change| change.matches_rev(&session.target.rev))
                    .or_else(|| {
                        (session.target.rev == "@" && !stack.is_empty()).then(|| stack.len() - 1)
                    });
                Ok(json!({
                    "base": session.target.base,
                    "revision": session.target.rev,
                    "changes": stack
                        .iter()
                        .enumerate()
                        .map(|(index, change)| json!({
                            "change_id": change.change_id,
                            "bookmarks": change.bookmarks,
                            "description": change.description,
                            "current": current == Some(index),
                        }))
                        .collect::<Vec<_>>(),
                }))
            }
            // One change's own diff against its parent. Line numbers here
            // are what chunk parts carrying this change_id must reference.
            "review/change_diff" => {
                let change_id = require_str(params, "change_id")?;
                if change_id.trim().is_empty() {
                    return Err("change_id must not be empty".to_owned());
                }
                let jj = self.require_jj()?;
                let target = ReviewTarget::new(format!("{change_id}-"), change_id.clone());
                let raw = jj
                    .diff(&session.repo, &target)
                    .map_err(|error| format!("failed to read diff for {change_id}: {error}"))?;
                let files = crate::diff::DiffSet::parse(&raw)
                    .map(|diff| {
                        diff.files
                            .iter()
                            .map(|file| {
                                json!({
                                    "path": file.path,
                                    "status": file.status.to_string(),
                                    "additions": file.additions,
                                    "deletions": file.deletions,
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Ok(json!({
                    "change_id": change_id,
                    "base": target.base,
                    "revision": target.rev,
                    "files": files,
                    "raw": raw,
                }))
            }
            // What the human is looking at right now: selected file, focused
            // pane, and (when the diff cursor sits on an anchorable row) the
            // exact line/hunk. Served live through the TUI socket; a
            // standalone snapshot server reports its initial selection.
            "review/current_focus" => {
                let anchor = session.selected_line_anchor();
                let line = anchor.as_ref().and_then(|anchor| match anchor {
                    CommentAnchor::Line {
                        side,
                        old_line,
                        new_line,
                        hunk_header,
                        ..
                    } => Some(json!({
                        "side": side,
                        "old_line": old_line,
                        "new_line": new_line,
                        "hunk_header": hunk_header,
                    })),
                    _ => None,
                });
                Ok(json!({
                    "repo": session.repo.display().to_string(),
                    "base": session.target.base,
                    "revision": session.target.rev,
                    "pane": match session.focus {
                        Focus::Files => "files",
                        Focus::Diff => "diff",
                    },
                    "path": session.selected_file().map(|file| file.path.clone()),
                    "line": line,
                }))
            }
            "review/set_ordering" => {
                let paths = params
                    .get("paths")
                    .and_then(Value::as_array)
                    .ok_or("missing array param: paths")?
                    .iter()
                    .map(|path| {
                        path.as_str()
                            .map(str::to_owned)
                            .ok_or("paths must be strings")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let unknown: Vec<&String> = paths
                    .iter()
                    .filter(|path| session.files.iter().all(|file| &file.path != *path))
                    .collect();
                if !unknown.is_empty() {
                    return Err(format!("unknown files in ordering: {unknown:?}"));
                }
                self.overlay.ordering = paths;
                self.save_overlay()?;
                Ok(json!({ "ordering": self.overlay.ordering }))
            }
            "review/flag_section" => {
                let path = require_str(params, "path")?;
                let reason = require_str(params, "reason")?;
                let priority = match params.get("priority").and_then(Value::as_str) {
                    None => FlagPriority::default(),
                    Some(raw) => serde_json::from_value(json!(raw))
                        .map_err(|_| format!("invalid priority: {raw}"))?,
                };
                let flag = AgentFlag {
                    id: uuid::Uuid::new_v4().to_string(),
                    path,
                    line: params
                        .get("line")
                        .and_then(Value::as_u64)
                        .map(|line| line as usize),
                    reason,
                    priority,
                };
                self.overlay.flags.push(flag.clone());
                self.save_overlay()?;
                Ok(json!({ "id": flag.id }))
            }
            "review/set_chunks" => {
                let chunks = params
                    .get("chunks")
                    .and_then(Value::as_array)
                    .ok_or("missing array param: chunks")?
                    .iter()
                    .map(parse_chunk)
                    .collect::<Result<Vec<_>, String>>()?;
                let context = self.chunk_validation_context(session, &chunks)?;
                replace_review_chunks(&mut self.overlay.chunks, chunks, &context).map_err(
                    |invalid| {
                        format!(
                            "invalid chunk part(s): {}",
                            invalid_chunk_parts_message(&invalid)
                        )
                    },
                )?;
                self.save_overlay()?;
                Ok(json!({ "chunks": self.overlay.chunks.len() }))
            }
            "review/update_chunks" => {
                let chunks = params
                    .get("chunks")
                    .and_then(Value::as_array)
                    .ok_or("missing array param: chunks")?
                    .iter()
                    .map(parse_chunk)
                    .collect::<Result<Vec<_>, String>>()?;
                let context = self.chunk_validation_context(session, &chunks)?;
                let summary = update_review_chunks(&mut self.overlay.chunks, chunks, &context)
                    .map_err(|invalid| {
                        format!(
                            "invalid chunk part(s): {}",
                            invalid_chunk_parts_message(&invalid)
                        )
                    })?;
                self.save_overlay()?;
                Ok(
                    json!({ "chunks": summary.chunks, "updated": summary.updated, "added": summary.added }),
                )
            }
            "review/remove_chunks" => {
                let ids = params
                    .get("ids")
                    .and_then(Value::as_array)
                    .ok_or("missing array param: ids")?
                    .iter()
                    .map(|id| {
                        id.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| "ids must be strings".to_owned())
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                let summary = remove_review_chunks(&mut self.overlay.chunks, &ids)
                    .map_err(|unknown| format!("unknown chunk id(s): {}", unknown.join(", ")))?;
                self.save_overlay()?;
                Ok(json!({ "chunks": summary.chunks, "removed": summary.removed }))
            }
            // Per-change briefings, one per change in the stack. The zen
            // walkthrough shows each as a chapter intro card before that
            // change's stops. Replaces the previous set.
            "review/set_change_briefs" => {
                let briefs = params
                    .get("briefs")
                    .and_then(Value::as_array)
                    .ok_or("missing array param: briefs")?
                    .iter()
                    .map(parse_change_brief)
                    .collect::<Result<Vec<_>, String>>()?;
                self.overlay.briefs = briefs;
                self.save_overlay()?;
                Ok(json!({ "briefs": self.overlay.briefs.len() }))
            }
            "review/draft_comment" => {
                let path = require_str(params, "path")?;
                let body = require_str(params, "body")?;
                let draft = AgentDraft {
                    id: uuid::Uuid::new_v4().to_string(),
                    path,
                    line: params
                        .get("line")
                        .and_then(Value::as_u64)
                        .map(|line| line as usize),
                    body,
                    state: DraftState::Pending,
                    accepted_comment_id: None,
                };
                self.overlay.drafts.push(draft.clone());
                self.save_overlay()?;
                Ok(json!({ "id": draft.id }))
            }
            other => Err(format!("unknown method: {other}")),
        }
    }

    fn require_jj(&self) -> Result<&(dyn JjBackend + Send), String> {
        self.jj
            .as_deref()
            .ok_or_else(|| "jj backend unavailable in this server".to_owned())
    }

    fn save_overlay(&mut self) -> Result<(), String> {
        // Merge dispositions the TUI may have written since our last write:
        // draft states/accepted ids flow TUI -> agent, everything else
        // agent -> TUI.
        if let Ok(on_disk) = AgentOverlay::load_or_default(&self.overlay_path) {
            for draft in &mut self.overlay.drafts {
                if let Some(disk_draft) = on_disk.drafts.iter().find(|disk| disk.id == draft.id)
                    && draft.state == DraftState::Pending
                {
                    draft.state = disk_draft.state;
                    draft.accepted_comment_id = disk_draft.accepted_comment_id.clone();
                }
            }
        }
        self.overlay
            .save(&self.overlay_path)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn chunk_validation_context(
        &self,
        session: &ReviewSession,
        chunks: &[ReviewChunk],
    ) -> Result<ChunkValidationContext<'static>, String> {
        let session_files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let mut parsed_changes = Vec::new();
        if chunks.iter().any(|chunk| chunk.change_id.is_some()) {
            let jj = self.require_jj()?;
            for change_id in chunks.iter().filter_map(|chunk| chunk.change_id.as_ref()) {
                if parsed_changes
                    .iter()
                    .any(|(existing, _): &(String, crate::diff::DiffSet)| existing == change_id)
                {
                    continue;
                }
                let target = ReviewTarget::new(format!("{change_id}-"), change_id.clone());
                if let Ok(raw) = jj.diff(&session.repo, &target)
                    && let Ok(diff) = crate::diff::DiffSet::parse(&raw)
                {
                    parsed_changes.push((change_id.clone(), diff));
                }
            }
        }
        let leaked_session: &'static [crate::diff::FileDiff] =
            Box::leak(session_files.into_boxed_slice());
        let leaked_changes: &'static [(String, crate::diff::DiffSet)] =
            Box::leak(parsed_changes.into_boxed_slice());
        let change_diffs = leaked_changes
            .iter()
            .map(|(change_id, diff)| ChangeDiffContext {
                change_id: change_id.clone(),
                files: &diff.files,
            })
            .collect::<Vec<_>>();
        Ok(ChunkValidationContext {
            session_files: leaked_session,
            change_diffs,
        })
    }
}

fn parse_chunk(value: &Value) -> Result<ReviewChunk, String> {
    let title = require_str(value, "title")?;
    let parts = value
        .get("parts")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .map(|part| {
                    Ok(ChunkPart {
                        path: require_str(part, "path")?,
                        start_line: part
                            .get("start_line")
                            .and_then(Value::as_u64)
                            .map(|line| line as usize),
                        end_line: part
                            .get("end_line")
                            .and_then(Value::as_u64)
                            .map(|line| line as usize),
                    })
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(ReviewChunk {
        id: value
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        title,
        importance: parse_chunk_importance(value.get("importance").and_then(Value::as_str))?,
        change_id: value
            .get("change_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|change_id| !change_id.is_empty())
            .map(str::to_owned),
        rationale: value
            .get("rationale")
            .and_then(Value::as_str)
            .map(str::to_owned),
        explanation: value
            .get("explanation")
            .and_then(Value::as_str)
            .map(str::to_owned),
        artifacts: parse_artifacts(value)?,
        parts,
    })
}

fn parse_change_brief(value: &Value) -> Result<ChangeBrief, String> {
    let change_id = require_str(value, "change_id")?;
    let change_id = change_id.trim();
    if change_id.is_empty() {
        return Err("change_id must not be empty".to_owned());
    }
    let summary = require_str(value, "summary")?;
    if summary.trim().is_empty() {
        return Err("summary must not be empty".to_owned());
    }
    Ok(ChangeBrief {
        change_id: change_id.to_owned(),
        summary,
        artifacts: parse_artifacts(value)?,
    })
}

fn parse_artifacts(value: &Value) -> Result<Vec<Artifact>, String> {
    value
        .get("artifacts")
        .and_then(Value::as_array)
        .map(|artifacts| {
            artifacts
                .iter()
                .map(|artifact| {
                    let title = require_str(artifact, "title")?;
                    if title.trim().is_empty() {
                        return Err("artifact title must not be empty".to_owned());
                    }
                    let body = require_str(artifact, "body")?;
                    if body.trim().is_empty() {
                        return Err("artifact body must not be empty".to_owned());
                    }
                    Ok(Artifact {
                        title,
                        kind: parse_artifact_kind(artifact.get("kind").and_then(Value::as_str))?,
                        body,
                    })
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

fn parse_artifact_kind(value: Option<&str>) -> Result<ArtifactKind, String> {
    match value.unwrap_or("example").to_ascii_lowercase().as_str() {
        "example" | "usage" => Ok(ArtifactKind::Example),
        "output" | "run" | "result" => Ok(ArtifactKind::Output),
        "diagram" | "chart" => Ok(ArtifactKind::Diagram),
        "note" | "text" => Ok(ArtifactKind::Note),
        other => Err(format!(
            "invalid artifact kind: {other} (expected example, output, diagram, or note)"
        )),
    }
}

fn parse_chunk_importance(value: Option<&str>) -> Result<ChunkImportance, String> {
    match value.unwrap_or("spotlight").to_ascii_lowercase().as_str() {
        "spotlight" | "tour" | "focus" | "important" => Ok(ChunkImportance::Spotlight),
        "glance" | "skim" | "overview" | "routine" => Ok(ChunkImportance::Glance),
        other => Err(format!(
            "invalid chunk importance: {other} (expected spotlight or glance)"
        )),
    }
}

fn require_str(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("missing string param: {key}"))
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

/// Unix-socket hosting: the TUI binds the workspace's runtime-dir socket
/// (see [`crate::paths`]) and answers requests from its event loop, so
/// agents talk to the *live* session.
#[cfg(unix)]
pub mod socket {
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        os::unix::net::{UnixListener, UnixStream},
        path::{Path, PathBuf},
        sync::mpsc,
        thread,
    };

    use color_eyre::eyre::{Context, Result, bail};

    use super::AcpHandler;
    use crate::{app::ReviewSession, jj::JjBackend};

    /// One JSON-RPC line from a connected agent, plus where to send the
    /// response. `None` responses (notifications) send nothing.
    pub struct AcpSocketRequest {
        line: String,
        reply: mpsc::Sender<Option<String>>,
    }

    /// Live ACP host owned by the TUI: a listener thread feeds requests
    /// through a channel; the TUI event loop drains them between input
    /// events via [`AcpBridge::process_pending`].
    pub struct AcpBridge {
        handler: AcpHandler,
        receiver: mpsc::Receiver<AcpSocketRequest>,
        socket_path: PathBuf,
    }

    impl AcpBridge {
        /// Bind the socket and spawn the accept loop. Fails if another live
        /// server is already bound; silently replaces a stale socket file
        /// left behind by a crashed process. `jj` enables the stack-aware
        /// methods (`review/stack_changes`, `review/change_diff`).
        pub fn bind(
            socket_path: PathBuf,
            overlay_path: PathBuf,
            jj: Option<Box<dyn JjBackend + Send>>,
        ) -> Result<Self> {
            if let Some(parent) = socket_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            let listener = match UnixListener::bind(&socket_path) {
                Ok(listener) => listener,
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                    if UnixStream::connect(&socket_path).is_ok() {
                        bail!(
                            "another ACP server is already listening on {}",
                            socket_path.display()
                        );
                    }
                    // Stale socket from a dead process: replace it.
                    fs::remove_file(&socket_path).with_context(|| {
                        format!("failed to remove stale socket {}", socket_path.display())
                    })?;
                    UnixListener::bind(&socket_path).with_context(|| {
                        format!("failed to bind ACP socket {}", socket_path.display())
                    })?
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("failed to bind ACP socket {}", socket_path.display())
                    });
                }
            };

            let (sender, receiver) = mpsc::channel();
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let sender = sender.clone();
                    thread::spawn(move || serve_connection(stream, &sender));
                }
            });

            Ok(Self {
                handler: {
                    let mut handler = AcpHandler::new(overlay_path)?;
                    handler.mark_live_session();
                    if let Some(jj) = jj {
                        handler.set_jj_backend(jj);
                    }
                    handler
                },
                receiver,
                socket_path,
            })
        }

        /// Answer all queued agent requests against the live session and
        /// apply any overlay changes to it. Returns whether the overlay
        /// changed (i.e. an agent wrote suggestions); latency is bounded by
        /// the event-loop tick.
        pub fn process_pending(&mut self, session: &mut ReviewSession) -> bool {
            let mut overlay_changed = false;
            while let Ok(request) = self.receiver.try_recv() {
                // Pick up dispositions the TUI wrote since the last request
                // so reads (review/overlay) are never stale.
                self.handler.refresh_overlay();
                let before = self.handler.overlay().clone();
                let response = self.handler.handle_line(session, &request.line);
                if self.handler.overlay() != &before {
                    session.apply_agent_overlay(self.handler.overlay());
                    overlay_changed = true;
                }
                // A dropped receiver just means the agent hung up.
                let _ = request
                    .reply
                    .send(response.map(|response| response.to_string()));
            }
            overlay_changed
        }
    }

    impl Drop for AcpBridge {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.socket_path);
        }
    }

    /// Per-connection loop: forward lines to the TUI, write responses back.
    fn serve_connection(stream: UnixStream, sender: &mpsc::Sender<AcpSocketRequest>) {
        let Ok(read_half) = stream.try_clone() else {
            return;
        };
        let mut writer = stream;
        for line in BufReader::new(read_half).lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let (reply_sender, reply_receiver) = mpsc::channel();
            if sender
                .send(AcpSocketRequest {
                    line,
                    reply: reply_sender,
                })
                .is_err()
            {
                // TUI shut down; close the connection.
                break;
            }
            match reply_receiver.recv() {
                Ok(Some(response)) => {
                    if writeln!(writer, "{response}")
                        .and_then(|()| writer.flush())
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }
    }

    /// Bridge stdio to a live TUI socket: stdin lines go to the socket,
    /// socket lines go to stdout. Returns when both sides close. Used by
    /// `gander acp` so agent-spawned servers reach the live session.
    pub fn bridge_stdio(socket_path: &Path) -> Result<()> {
        let stream = UnixStream::connect(socket_path)
            .with_context(|| format!("failed to connect to {}", socket_path.display()))?;
        let mut write_half = stream.try_clone()?;
        let stdin_pump = thread::spawn(move || {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut write_half);
            // Propagate stdin EOF so the TUI-side connection loop ends.
            let _ = write_half.shutdown(std::net::Shutdown::Write);
        });
        let mut reader = stream;
        std::io::copy(&mut reader, &mut std::io::stdout().lock())?;
        let _ = stdin_pump.join();
        Ok(())
    }

    /// True if a live server currently accepts connections on `socket_path`.
    pub fn is_live(socket_path: &Path) -> bool {
        UnixStream::connect(socket_path).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{diff::DiffSet, jj::ReviewTarget, state::ReviewState};

    fn session(dir: &std::path::Path) -> ReviewSession {
        let diff = DiffSet::parse(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1 +1 @@
-old
+new
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-old title
+new title
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            dir.to_path_buf(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.add_comment("file note".into());
        session
    }

    fn server() -> (AcpServer, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let session = session(dir.path());
        let overlay_path = dir.path().join("agent.json");
        (AcpServer::new(session, overlay_path).unwrap(), dir)
    }

    /// A jj backend canned for stack-aware tests.
    struct MockJj;

    impl crate::jj::JjBackend for MockJj {
        fn diff(&self, _repo: &std::path::Path, target: &ReviewTarget) -> Result<String> {
            assert_eq!(target, &ReviewTarget::new("abc-", "abc"));
            Ok(r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1 +1 @@
-old
+new
"#
            .to_owned())
        }

        fn change_summaries(
            &self,
            _repo: &std::path::Path,
        ) -> Result<Vec<crate::jj::JjChangeSummary>> {
            Ok(Vec::new())
        }

        fn stack_changes(
            &self,
            _repo: &std::path::Path,
        ) -> Result<Vec<crate::jj::JjChangeSummary>> {
            Ok(vec![
                crate::jj::JjChangeSummary {
                    change_id: "abc".to_owned(),
                    bookmarks: "feature".to_owned(),
                    description: "feat: first".to_owned(),
                },
                crate::jj::JjChangeSummary {
                    change_id: "def".to_owned(),
                    bookmarks: String::new(),
                    description: "feat: second".to_owned(),
                },
            ])
        }

        fn change_fingerprint(
            &self,
            _repo: &std::path::Path,
            _target: &ReviewTarget,
        ) -> Result<String> {
            Ok(String::new())
        }

        fn operations(
            &self,
            _repo: &std::path::Path,
        ) -> Result<Vec<crate::jj::JjOperationSummary>> {
            Ok(Vec::new())
        }

        fn diff_at_operation(
            &self,
            _repo: &std::path::Path,
            _target: &ReviewTarget,
            _operation_id: &str,
        ) -> Result<String> {
            Ok(String::new())
        }

        fn file_contents(
            &self,
            _repo: &std::path::Path,
            _rev: &str,
            _path: &str,
        ) -> Result<String> {
            Ok(String::new())
        }

        fn run_command(&self, _repo: &std::path::Path, _args: &[String]) -> Result<String> {
            Ok(String::new())
        }
    }

    fn call(server: &mut AcpServer, method: &str, params: Value) -> Value {
        let request =
            json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string();
        let response = server.handle_line(&request).unwrap();
        assert!(
            response.get("error").is_none(),
            "unexpected error: {response}"
        );
        response["result"].clone()
    }

    #[test]
    fn initialize_reports_protocol_and_capabilities() {
        let (mut server, _dir) = server();

        let result = call(&mut server, "initialize", Value::Null);

        assert_eq!(result["protocol"], "gander-acp");
        assert_eq!(result["version"], ACP_PROTOCOL_VERSION);
        assert!(
            result["capabilities"]
                .as_array()
                .unwrap()
                .contains(&json!("review/draft_comment"))
        );
    }

    #[test]
    fn read_methods_expose_files_diff_and_comments() {
        let (mut server, _dir) = server();

        let files = call(&mut server, "review/files", Value::Null);
        let paths: Vec<&str> = files
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, ["src/app.rs", "README.md"]);
        assert_eq!(files[0]["viewed"], false);

        let diff = call(
            &mut server,
            "review/file_diff",
            json!({ "path": "src/app.rs" }),
        );
        assert!(diff["raw"].as_str().unwrap().contains("+new"));
        assert!(diff["fingerprint"].as_str().unwrap().len() > 10);

        let comments = call(&mut server, "review/comments", Value::Null);
        assert_eq!(comments.as_array().unwrap().len(), 1);
        assert_eq!(comments[0]["body"], "file note");
        assert_eq!(comments[0]["state"], "draft");
    }

    #[test]
    fn current_focus_reports_selected_file_and_pane() {
        let (mut server, _dir) = server();

        let focus = call(&mut server, "review/current_focus", Value::Null);

        assert_eq!(focus["path"], "src/app.rs");
        assert_eq!(focus["pane"], "files");
        assert_eq!(focus["base"], "trunk()");
        assert_eq!(focus["revision"], "@");
        assert!(focus.get("line").is_some());
    }

    #[test]
    fn write_methods_persist_overlay_to_disk() {
        let (mut server, dir) = server();

        call(
            &mut server,
            "review/set_ordering",
            json!({ "paths": ["README.md", "src/app.rs"] }),
        );
        let flag = call(
            &mut server,
            "review/flag_section",
            json!({ "path": "src/app.rs", "line": 1, "reason": "risky", "priority": "critical" }),
        );
        call(
            &mut server,
            "review/set_chunks",
            json!({ "chunks": [
                { "title": "core change", "parts": [{ "path": "src/app.rs", "start_line": 1, "end_line": 1 }] }
            ] }),
        );
        let draft = call(
            &mut server,
            "review/draft_comment",
            json!({ "path": "README.md", "body": "typo in the title" }),
        );

        let overlay = AgentOverlay::load_or_default(&dir.path().join("agent.json")).unwrap();
        assert_eq!(overlay.ordering, ["README.md", "src/app.rs"]);
        assert_eq!(overlay.flags.len(), 1);
        assert_eq!(overlay.flags[0].id, flag["id"].as_str().unwrap());
        assert_eq!(overlay.flags[0].priority, FlagPriority::Critical);
        assert_eq!(overlay.chunks.len(), 1);
        assert_eq!(overlay.chunks[0].title, "core change");
        assert_eq!(overlay.drafts.len(), 1);
        assert_eq!(overlay.drafts[0].id, draft["id"].as_str().unwrap());
        assert_eq!(overlay.drafts[0].state, DraftState::Pending);
    }

    #[test]
    fn update_and_remove_chunks_are_incremental_and_strict() {
        let (mut server, dir) = server();
        call(
            &mut server,
            "review/set_chunks",
            json!({ "chunks": [
                { "id": "a", "title": "first", "parts": [{ "path": "src/app.rs", "start_line": 1, "end_line": 1 }] },
                { "id": "b", "title": "second", "parts": [{ "path": "README.md", "start_line": 1, "end_line": 1 }] }
            ] }),
        );
        let result = call(
            &mut server,
            "review/update_chunks",
            json!({ "chunks": [
                { "id": "a", "title": "updated", "importance": "glance", "parts": [{ "path": "src/app.rs", "start_line": 1, "end_line": 1 }] },
                { "id": "c", "title": "third", "parts": [{ "path": "README.md", "start_line": 1, "end_line": 1 }] }
            ] }),
        );
        assert_eq!(result, json!({ "chunks": 3, "updated": 1, "added": 1 }));
        let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "review/remove_chunks", "params": { "ids": ["missing"] } }).to_string();
        let err = server.handle_line(&request).unwrap();
        assert!(
            err["error"]["message"]
                .as_str()
                .unwrap()
                .contains("unknown chunk id")
        );
        let result = call(&mut server, "review/remove_chunks", json!({ "ids": ["b"] }));
        assert_eq!(result, json!({ "chunks": 2, "removed": 1 }));
        let overlay = AgentOverlay::load_or_default(&dir.path().join("agent.json")).unwrap();
        assert_eq!(
            overlay
                .chunks
                .iter()
                .map(|chunk| chunk.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "c"]
        );
        assert_eq!(overlay.chunks[0].title, "updated");
    }

    #[test]
    fn stack_methods_error_without_a_jj_backend() {
        let (mut server, _dir) = server();

        for method in ["review/stack_changes", "review/change_diff"] {
            let request = json!({
                "jsonrpc": "2.0", "id": 5,
                "method": method,
                "params": { "change_id": "abc" },
            })
            .to_string();
            let response = server.handle_line(&request).unwrap();
            assert!(
                response["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("jj backend unavailable"),
                "{method} should fail without a backend"
            );
        }
    }

    #[test]
    fn stack_changes_lists_the_stack_and_marks_the_current_change() {
        let (server, _dir) = server();
        let mut server = server.with_jj(Box::new(MockJj));

        let result = call(&mut server, "review/stack_changes", Value::Null);

        assert_eq!(result["base"], "trunk()");
        assert_eq!(result["revision"], "@");
        let changes = result["changes"].as_array().unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0]["change_id"], "abc");
        assert_eq!(changes[0]["description"], "feat: first");
        assert_eq!(changes[0]["current"], false);
        // Reviewing `@`: the working-copy change is the current one.
        assert_eq!(changes[1]["current"], true);
    }

    #[test]
    fn change_diff_serves_one_change_against_its_parent() {
        let (server, _dir) = server();
        let mut server = server.with_jj(Box::new(MockJj));

        let result = call(
            &mut server,
            "review/change_diff",
            json!({ "change_id": "abc" }),
        );

        assert_eq!(result["change_id"], "abc");
        assert_eq!(result["base"], "abc-");
        assert_eq!(result["revision"], "abc");
        assert!(result["raw"].as_str().unwrap().contains("+new"));
        assert_eq!(result["files"][0]["path"], "src/app.rs");
        assert_eq!(result["files"][0]["additions"], 1);
    }

    #[test]
    fn set_chunks_records_change_anchors() {
        let (server, dir) = server();
        let mut server = server.with_jj(Box::new(MockJj));

        call(
            &mut server,
            "review/set_chunks",
            json!({ "chunks": [
                {
                    "title": "stacked change",
                    "change_id": "abc",
                    "parts": [{ "path": "src/app.rs", "start_line": 1, "end_line": 1 }],
                },
                { "title": "unanchored", "change_id": "  ", "parts": [] },
            ] }),
        );

        let overlay = AgentOverlay::load_or_default(&dir.path().join("agent.json")).unwrap();
        assert_eq!(overlay.chunks[0].change_id.as_deref(), Some("abc"));
        // Blank anchors normalize to None instead of a whitespace revset.
        assert_eq!(overlay.chunks[1].change_id, None);
    }

    #[test]
    fn set_chunks_rejects_invalid_part_and_applies_nothing() {
        let (mut server, dir) = server();
        call(
            &mut server,
            "review/set_chunks",
            json!({ "chunks": [
                { "title": "valid", "parts": [{ "path": "src/app.rs", "start_line": 1, "end_line": 1 }] }
            ] }),
        );
        let request = json!({
            "jsonrpc": "2.0", "id": 9,
            "method": "review/set_chunks",
            "params": { "chunks": [
                { "title": "bad", "parts": [{ "path": "missing.rs", "start_line": 1, "end_line": 1 }] }
            ] }
        })
        .to_string();
        let response = server.handle_line(&request).unwrap();
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("invalid chunk part")
        );
        let overlay = AgentOverlay::load_or_default(&dir.path().join("agent.json")).unwrap();
        assert_eq!(overlay.chunks.len(), 1);
        assert_eq!(overlay.chunks[0].title, "valid");
    }

    #[test]
    fn set_change_briefs_replaces_and_validates() {
        let (mut server, dir) = server();

        call(
            &mut server,
            "review/set_change_briefs",
            json!({ "briefs": [
                { "change_id": " abc ", "summary": "Lays the groundwork for the retry loop." },
            ] }),
        );
        let overlay = AgentOverlay::load_or_default(&dir.path().join("agent.json")).unwrap();
        assert_eq!(overlay.briefs.len(), 1);
        // Whitespace around the change id normalizes away.
        assert_eq!(overlay.briefs[0].change_id, "abc");

        // A later call replaces the previous set wholesale.
        call(
            &mut server,
            "review/set_change_briefs",
            json!({ "briefs": [
                { "change_id": "def", "summary": "Builds the retry loop on the groundwork." },
            ] }),
        );
        let overlay = AgentOverlay::load_or_default(&dir.path().join("agent.json")).unwrap();
        assert_eq!(overlay.briefs.len(), 1);
        assert_eq!(overlay.briefs[0].change_id, "def");

        for bad in [
            json!({}),
            json!({ "briefs": [{ "change_id": "  ", "summary": "x" }] }),
            json!({ "briefs": [{ "change_id": "abc", "summary": "   " }] }),
        ] {
            let request = json!({
                "jsonrpc": "2.0", "id": 9,
                "method": "review/set_change_briefs",
                "params": bad,
            })
            .to_string();
            let response = server.handle_line(&request).unwrap();
            assert!(response.get("error").is_some(), "expected error for {bad}");
        }
    }

    #[test]
    fn chunks_and_briefs_carry_artifacts() {
        let (mut server, dir) = server();

        call(
            &mut server,
            "review/set_chunks",
            json!({ "chunks": [
                {
                    "title": "core change",
                    "parts": [{ "path": "src/app.rs", "start_line": 1, "end_line": 1 }],
                    "artifacts": [
                        { "title": "usage", "body": "let app = App::new();" },
                        { "title": "test run", "kind": "output", "body": "3 passed" },
                    ],
                }
            ] }),
        );
        call(
            &mut server,
            "review/set_change_briefs",
            json!({ "briefs": [
                {
                    "change_id": "abc",
                    "summary": "Reworks the core loop.",
                    "artifacts": [{ "title": "flow", "kind": "diagram", "body": "a -> b" }],
                },
            ] }),
        );

        let overlay = AgentOverlay::load_or_default(&dir.path().join("agent.json")).unwrap();
        let artifacts = &overlay.chunks[0].artifacts;
        assert_eq!(artifacts.len(), 2);
        // Kind defaults to `example` when omitted.
        assert_eq!(artifacts[0].kind, ArtifactKind::Example);
        assert_eq!(artifacts[1].kind, ArtifactKind::Output);
        assert_eq!(overlay.briefs[0].artifacts.len(), 1);
        assert_eq!(overlay.briefs[0].artifacts[0].kind, ArtifactKind::Diagram);

        for bad in [
            json!({ "chunks": [{ "title": "x", "artifacts": [{ "title": " ", "body": "y" }] }] }),
            json!({ "chunks": [{ "title": "x", "artifacts": [{ "title": "y", "body": "  " }] }] }),
            json!({ "chunks": [{ "title": "x", "artifacts": [{ "title": "y", "kind": "movie", "body": "z" }] }] }),
        ] {
            let request = json!({
                "jsonrpc": "2.0", "id": 11,
                "method": "review/set_chunks",
                "params": bad,
            })
            .to_string();
            let response = server.handle_line(&request).unwrap();
            assert!(response.get("error").is_some(), "expected error for {bad}");
        }
    }

    #[test]
    fn set_ordering_rejects_unknown_files() {
        let (mut server, _dir) = server();

        let request = json!({
            "jsonrpc": "2.0", "id": 7,
            "method": "review/set_ordering",
            "params": { "paths": ["nope.rs"] },
        })
        .to_string();
        let response = server.handle_line(&request).unwrap();

        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("unknown files")
        );
    }

    #[test]
    fn server_merges_tui_dispositions_instead_of_clobbering() {
        let (mut server, dir) = server();
        let draft = call(
            &mut server,
            "review/draft_comment",
            json!({ "path": "README.md", "body": "first" }),
        );
        let draft_id = draft["id"].as_str().unwrap().to_owned();

        // Simulate the TUI accepting the draft on disk.
        let overlay_path = dir.path().join("agent.json");
        let mut on_disk = AgentOverlay::load_or_default(&overlay_path).unwrap();
        on_disk.drafts[0].state = DraftState::Accepted;
        on_disk.drafts[0].accepted_comment_id = Some("comment-1".to_owned());
        on_disk.save(&overlay_path).unwrap();

        // A subsequent agent write must not clobber the disposition.
        call(
            &mut server,
            "review/draft_comment",
            json!({ "path": "README.md", "body": "second" }),
        );

        let merged = AgentOverlay::load_or_default(&overlay_path).unwrap();
        let first = merged
            .drafts
            .iter()
            .find(|draft| draft.id == draft_id)
            .unwrap();
        assert_eq!(first.state, DraftState::Accepted);
        assert_eq!(first.accepted_comment_id.as_deref(), Some("comment-1"));
        assert_eq!(merged.drafts.len(), 2);
    }

    #[test]
    fn unknown_methods_and_parse_errors_return_jsonrpc_errors() {
        let (mut server, _dir) = server();

        let response = server
            .handle_line(&json!({ "jsonrpc": "2.0", "id": 1, "method": "nope" }).to_string())
            .unwrap();
        assert_eq!(response["error"]["code"], -32000);

        let response = server.handle_line("{not json").unwrap();
        assert_eq!(response["error"]["code"], -32700);

        // Notifications (no id) never get responses.
        assert!(
            server
                .handle_line(&json!({ "jsonrpc": "2.0", "method": "initialize" }).to_string())
                .is_none()
        );
    }

    #[test]
    fn serve_loop_round_trips_over_buffers() {
        let (mut server, _dir) = server();
        let input = format!(
            "{}\n{}\n",
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "review/summary" }),
        );
        let mut output = Vec::new();

        server
            .serve(std::io::Cursor::new(input), &mut output)
            .unwrap();

        let lines: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["id"], 1);
        assert_eq!(lines[1]["result"]["base"], "trunk()");
    }

    #[cfg(unix)]
    mod socket_tests {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        use super::*;
        use crate::acp::socket::AcpBridge;

        /// Drive the bridge like the TUI event loop until the client's
        /// request has been answered.
        fn pump_until<T>(
            bridge: &mut AcpBridge,
            session: &mut ReviewSession,
            mut ready: impl FnMut() -> Option<T>,
        ) -> T {
            for _ in 0..200 {
                bridge.process_pending(session);
                if let Some(value) = ready() {
                    return value;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("socket request was never answered");
        }

        fn call_over_socket(
            bridge: &mut AcpBridge,
            session: &mut ReviewSession,
            client: &mut BufReader<UnixStream>,
            request: &Value,
        ) -> Value {
            client
                .get_mut()
                .write_all(format!("{request}\n").as_bytes())
                .unwrap();
            // Reads from the socket block, so poll the bridge from a helper
            // thread reading in the background is overkill: instead pump the
            // bridge, then do a blocking read once the reply is queued.
            let response = std::sync::Arc::new(std::sync::Mutex::new(Option::<String>::None));
            let response_in_thread = response.clone();
            let mut stream = client.get_ref().try_clone().unwrap();
            let reader_thread = std::thread::spawn(move || {
                let mut line = String::new();
                let mut reader = BufReader::new(&mut stream);
                if reader.read_line(&mut line).is_ok() && !line.is_empty() {
                    *response_in_thread.lock().unwrap() = Some(line);
                }
            });
            let line = pump_until(bridge, session, || response.lock().unwrap().take());
            reader_thread.join().unwrap();
            serde_json::from_str(&line).unwrap()
        }

        #[test]
        fn socket_serves_live_session_and_applies_writes() {
            let dir = tempfile::tempdir().unwrap();
            let mut session = session(dir.path());
            let socket_path = dir.path().join("acp.sock");
            let overlay_path = dir.path().join("agent.json");
            let mut bridge =
                AcpBridge::bind(socket_path.clone(), overlay_path.clone(), None).unwrap();
            assert!(socket::is_live(&socket_path));

            let mut client = BufReader::new(UnixStream::connect(&socket_path).unwrap());

            let response = call_over_socket(
                &mut bridge,
                &mut session,
                &mut client,
                &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
            );
            assert_eq!(response["result"]["protocol"], "gander-acp");

            // Live read: session mutations made "in the TUI" are visible.
            session.mark_all_viewed();
            let response = call_over_socket(
                &mut bridge,
                &mut session,
                &mut client,
                &json!({ "jsonrpc": "2.0", "id": 2, "method": "review/files" }),
            );
            assert_eq!(response["result"][0]["viewed"], true);

            // Write: applied to the live session and persisted to disk.
            let response = call_over_socket(
                &mut bridge,
                &mut session,
                &mut client,
                &json!({
                    "jsonrpc": "2.0", "id": 3,
                    "method": "review/draft_comment",
                    "params": { "path": "README.md", "body": "typo" },
                }),
            );
            let draft_id = response["result"]["id"].as_str().unwrap();
            assert!(
                session
                    .agent_drafts
                    .iter()
                    .any(|draft| draft.id == draft_id)
            );
            let overlay = AgentOverlay::load_or_default(&overlay_path).unwrap();
            assert_eq!(overlay.drafts.len(), 1);
        }

        #[test]
        fn bind_rejects_live_socket_but_replaces_stale_one() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("acp.sock");
            let overlay_path = dir.path().join("agent.json");

            let live = AcpBridge::bind(socket_path.clone(), overlay_path.clone(), None).unwrap();
            let Err(error) = AcpBridge::bind(socket_path.clone(), overlay_path.clone(), None)
            else {
                panic!("second bind must fail while the first is live");
            };
            assert!(error.to_string().contains("already listening"));
            drop(live);

            // Dropping removed the socket file; a stale file also rebinds.
            std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
            assert!(!socket_path.exists());
            let stale = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
            drop(stale);
            assert!(socket_path.exists());
            // macOS can briefly complete connects against a freshly closed
            // listener's backlog; wait until the socket reads as dead like a
            // genuinely stale one would.
            for _ in 0..200 {
                if !socket::is_live(&socket_path) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(!socket::is_live(&socket_path));
            AcpBridge::bind(socket_path, overlay_path, None).unwrap();
        }
    }
}
