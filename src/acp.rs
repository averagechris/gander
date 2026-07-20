//! Agent-collaborative review over ACP.
//!
//! `gander acp` exposes the current review session over a line-delimited
//! JSON-RPC 2.0 protocol on stdio (the transport used by the Agent Client
//! Protocol). Agents can read the diff, comments, and viewed state, and can
//! write non-comment review suggestions into the shared agent overlay and
//! agent-authored draft comments into durable review state.
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
    agent::{AgentFlag, AgentOverlay, FlagPriority},
    anchor::CommentAnchor,
    app::{Focus, ReviewSession},
    jj::{JjBackend, ReviewTarget},
};

pub const ACP_PROTOCOL_VERSION: u32 = 1;
const PRESENT_METHODS: &[&str] = &[
    "present/status",
    "present/start",
    "present/end",
    "present/next",
    "present/prev",
    "present/goto",
    "present/focus",
    "present/reload",
];

pub fn is_present_method(method: &str) -> bool {
    PRESENT_METHODS.contains(&method)
}

pub fn no_live_tui_error() -> String {
    "present/* methods require a live TUI; start one with `gander tui` and retry".to_owned()
}

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
    state_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewMutation {
    DraftComment {
        path: String,
        line: Option<usize>,
        body: String,
    },
}

#[derive(Debug)]
struct ParsedReviewMutation {
    id: Option<Value>,
    mutation: ReviewMutation,
}

impl AcpServer {
    pub fn new(session: ReviewSession, overlay_path: PathBuf) -> Result<Self> {
        Ok(Self {
            session,
            handler: AcpHandler::new(overlay_path)?,
            state_path: None,
        })
    }

    /// Enable the jj-backed stack methods (`review/stack_changes`,
    /// `review/change_diff`).
    pub fn with_jj(mut self, jj: Box<dyn JjBackend + Send>) -> Self {
        self.handler.set_jj_backend(jj);
        self
    }

    pub fn with_state_path(mut self, state_path: PathBuf) -> Self {
        self.state_path = Some(state_path);
        self
    }

    /// Serve requests until EOF. One JSON-RPC message per line.
    pub fn serve(&mut self, input: impl BufRead, mut output: impl Write) -> Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let response = match parse_review_mutation(&line) {
                Some(Ok(request)) => {
                    let result = self.apply_review_mutation(request.mutation);
                    review_mutation_response(request.id, result)
                }
                Some(Err((id, message))) => id.map(|id| error_response(id, -32602, &message)),
                None => self.handler.handle_line(&mut self.session, &line),
            };
            if let Some(response) = response {
                serde_json::to_writer(&mut output, &response)?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
        }
        Ok(())
    }

    fn apply_review_mutation(&mut self, mutation: ReviewMutation) -> Result<Value, String> {
        let state_path = self
            .state_path
            .as_deref()
            .ok_or_else(|| "durable review state path unavailable".to_owned())?;
        let mut latest = crate::state::ReviewState::load_or_default(state_path)
            .map_err(|error| error.to_string())?;
        let value = apply_review_mutation_to_state(&self.session, &mut latest, mutation)?;
        latest.save(state_path).map_err(|error| error.to_string())?;
        self.session.apply_review_state(latest);
        Ok(value)
    }

    #[cfg(test)]
    fn handle_line(&mut self, line: &str) -> Option<Value> {
        match parse_review_mutation(line) {
            Some(Ok(request)) => {
                let mut state = self.session.to_state();
                let result =
                    apply_review_mutation_to_state(&self.session, &mut state, request.mutation);
                if result.is_ok() {
                    self.session.apply_review_state(state);
                }
                review_mutation_response(request.id, result)
            }
            Some(Err((id, message))) => id.map(|id| error_response(id, -32602, &message)),
            None => self.handler.handle_line(&mut self.session, line),
        }
    }
}

fn apply_review_mutation_to_state(
    session: &ReviewSession,
    state: &mut crate::state::ReviewState,
    mutation: ReviewMutation,
) -> Result<Value, String> {
    match mutation {
        ReviewMutation::DraftComment { path, line, body } => session
            .add_agent_draft_to_state(state, path, line, body)
            .map(|comment| json!({ "id": comment.id }))
            .map_err(|error| error.to_string()),
    }
}

fn parse_review_mutation(
    line: &str,
) -> Option<Result<ParsedReviewMutation, (Option<Value>, String)>> {
    let request: Value = serde_json::from_str(line).ok()?;
    if request.get("method").and_then(Value::as_str) != Some("review/draft_comment") {
        return None;
    }
    let id = request.get("id").cloned();
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    let parsed = (|| {
        let path = require_str(&params, "path")?;
        let body = require_str(&params, "body")?;
        let line = params
            .get("line")
            .and_then(Value::as_u64)
            .map(|line| line as usize);
        Ok(ParsedReviewMutation {
            id: id.clone(),
            mutation: ReviewMutation::DraftComment { path, line, body },
        })
    })();
    Some(parsed.map_err(|message| (id, message)))
}

fn review_mutation_response(id: Option<Value>, result: Result<Value, String>) -> Option<Value> {
    id.map(|id| match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(message) => error_response(id, -32000, &message),
    })
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
    pub fn handle_line(&mut self, session: &mut ReviewSession, line: &str) -> Option<Value> {
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
        session: &mut ReviewSession,
        method: &str,
        params: &Value,
    ) -> Result<Value, String> {
        match method {
            "initialize" => Ok(json!({
                "protocol": "gander-acp",
                "version": ACP_PROTOCOL_VERSION,
                "mode": if self.live_session { "live-bridge" } else { "snapshot" },
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
                    "review/draft_comment",
                    "present/status",
                    "present/start",
                    "present/end",
                    "present/next",
                    "present/prev",
                    "present/goto",
                    "present/focus",
                    "present/reload",
                ],
            })),
            method if is_present_method(method) => Err(no_live_tui_error()),
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
            "review/comments" => Ok(comments_json(session.comments.iter())),
            // Internal compact handoff used by MCP durable comment mutations.
            // It guarantees capture comes from the same selected live/snapshot
            // session as MCP reads, without another jj query.
            "review/provenance_context" => Ok(json!({
                "repo": session.repo,
                "base": session.target.base,
                "revision": session.target.rev,
                "files": session.files.iter().map(|file| &file.diff).collect::<Vec<_>>(),
            })),
            "review/overlay" => {
                serde_json::to_value(&self.overlay).map_err(|error| error.to_string())
            }
            // The jj stack the review lives in (`trunk()..@`, oldest first).
            // The human often treats these as stacked PRs, so agents can
            // author durable walkthrough chapters change-by-change.
            "review/stack_changes" => {
                let jj = self.require_jj()?;
                let mut stack = jj
                    .stack_changes(&session.repo, &session.target)
                    .map_err(|error| format!("failed to load jj stack: {error}"))?;
                stack.retain(|change| !change.matches_rev(&session.target.base));
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
            // One change's own diff against its parent. Durable walkthrough
            // and attention targets can use its line numbers.
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
            other => Err(format!("unknown method: {other}")),
        }
    }

    fn require_jj(&self) -> Result<&(dyn JjBackend + Send), String> {
        self.jj
            .as_deref()
            .ok_or_else(|| "jj backend unavailable in this server".to_owned())
    }

    fn save_overlay(&mut self) -> Result<(), String> {
        self.overlay
            .save(&self.overlay_path)
            .map_err(|error| error.to_string())
    }
}

pub(crate) fn comments_json<'a>(
    comments: impl IntoIterator<Item = &'a crate::state::Comment>,
) -> Value {
    json!(
        comments
            .into_iter()
            .map(|comment| json!({
                "id": comment.id,
                "session_id": comment.session_id,
                "path": comment.path,
                "line": comment.line,
                "end_line": comment.end_line,
                "anchor": comment.anchor,
                "observation": comment.observation,
                "body": comment.body,
                "kind": comment.kind,
                "action": comment.action,
                "state": comment.state.label(),
                "author": comment.author,
                "channel": comment.channel,
                "replies": comment.replies,
                "created_at": comment.created_at,
                "updated_at": comment.updated_at,
            }))
            .collect::<Vec<_>>()
    )
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

    use serde_json::{Value, json};

    use super::{
        AcpHandler, ReviewMutation, error_response, is_present_method, parse_review_mutation,
    };
    use crate::{app::ReviewSession, jj::JjBackend};

    /// One JSON-RPC line from a connected agent, plus where to send the
    /// response. `None` responses (notifications) send nothing.
    pub struct AcpSocketRequest {
        line: String,
        reply: mpsc::Sender<Option<String>>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PresentCommand {
        Status,
        Start,
        End,
        Next,
        Prev,
        GotoIndex(usize),
        GotoStep(String),
        Focus {
            path: String,
            line: usize,
            end_line: Option<usize>,
            note: Option<String>,
        },
        Reload,
    }

    pub struct PresentRequest {
        pub id: Value,
        pub command: PresentCommand,
        reply: mpsc::Sender<Option<String>>,
    }

    impl PresentRequest {
        pub fn respond(self, result: Result<Value, (i64, String)>) {
            let response = match result {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": self.id, "result": result }),
                Err((code, message)) => error_response(self.id, code, &message),
            };
            let _ = self.reply.send(Some(response.to_string()));
        }
    }

    pub struct ReviewMutationRequest {
        pub id: Option<Value>,
        pub mutation: ReviewMutation,
        reply: mpsc::Sender<Option<String>>,
    }

    impl ReviewMutationRequest {
        pub fn respond(self, result: Result<Value, (i64, String)>) {
            let response = self.id.map(|id| match result {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err((code, message)) => error_response(id, code, &message),
            });
            let _ = self
                .reply
                .send(response.map(|response| response.to_string()));
        }
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
        #[cfg(test)]
        pub fn process_pending(&mut self, session: &mut ReviewSession, state_path: &Path) -> bool {
            let (overlay_changed, _had_requests, commands, mutations) =
                self.drain_ui_commands(session);
            for command in commands {
                command.respond(Err((
                    -32000,
                    "present command was not applied by the TUI".to_owned(),
                )));
            }
            for request in mutations {
                let result = crate::tui::persist_acp_review_mutation(
                    request.mutation.clone(),
                    session,
                    state_path,
                    &crate::state::ReviewStateTombstones::default(),
                )
                .map_err(|error| (-32000, error.to_string()));
                request.respond(result);
            }
            overlay_changed
        }

        pub fn drain_ui_commands(
            &mut self,
            session: &mut ReviewSession,
        ) -> (bool, bool, Vec<PresentRequest>, Vec<ReviewMutationRequest>) {
            let mut overlay_changed = false;
            let mut had_requests = false;
            let mut commands = Vec::new();
            let mut mutations = Vec::new();
            while let Ok(request) = self.receiver.try_recv() {
                had_requests = true;
                if let Some(command) = parse_present_request(&request.line, request.reply.clone()) {
                    commands.push(command);
                    continue;
                }
                match parse_review_mutation(&request.line) {
                    Some(Ok(parsed)) => {
                        mutations.push(ReviewMutationRequest {
                            id: parsed.id,
                            mutation: parsed.mutation,
                            reply: request.reply,
                        });
                        continue;
                    }
                    Some(Err((id, message))) => {
                        let response =
                            id.map(|id| error_response(id, -32602, &message).to_string());
                        let _ = request.reply.send(response);
                        continue;
                    }
                    None => {}
                }
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
            (overlay_changed, had_requests, commands, mutations)
        }
    }

    fn parse_present_request(
        line: &str,
        reply: mpsc::Sender<Option<String>>,
    ) -> Option<PresentRequest> {
        let request: Value = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(_) => return None,
        };
        let method = request.get("method").and_then(Value::as_str)?;
        if !is_present_method(method) {
            return None;
        }
        let id = request.get("id").cloned()?;
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let command = match method {
            "present/status" => PresentCommand::Status,
            "present/start" => PresentCommand::Start,
            "present/end" => PresentCommand::End,
            "present/next" => PresentCommand::Next,
            "present/prev" => PresentCommand::Prev,
            "present/reload" => PresentCommand::Reload,
            "present/goto" => {
                if let Some(index) = params.get("index").and_then(Value::as_u64) {
                    PresentCommand::GotoIndex(index as usize)
                } else if let Some(step) = params.get("step_id").and_then(Value::as_str) {
                    PresentCommand::GotoStep(step.to_owned())
                } else {
                    let _ = reply.send(Some(
                        error_response(id, -32602, "present/goto requires index or step_id")
                            .to_string(),
                    ));
                    return None;
                }
            }
            "present/focus" => {
                let Some(path) = params.get("path").and_then(Value::as_str) else {
                    let _ = reply.send(Some(
                        error_response(id, -32602, "present/focus requires path").to_string(),
                    ));
                    return None;
                };
                let Some(line) = params.get("line").and_then(Value::as_u64) else {
                    let _ = reply.send(Some(
                        error_response(id, -32602, "present/focus requires line").to_string(),
                    ));
                    return None;
                };
                PresentCommand::Focus {
                    path: path.to_owned(),
                    line: line as usize,
                    end_line: params
                        .get("end_line")
                        .and_then(Value::as_u64)
                        .map(|v| v as usize),
                    note: params
                        .get("note")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                }
            }
            _ => unreachable!(),
        };
        Some(PresentRequest { id, command, reply })
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
        fn snapshot_working_copy(&self, _repo: &std::path::Path) -> Result<()> {
            Ok(())
        }

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
            _target: &crate::jj::ReviewTarget,
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
        assert!(
            result["capabilities"]
                .as_array()
                .unwrap()
                .contains(&json!("present/status"))
        );
        let capabilities = result["capabilities"].as_array().unwrap();
        for removed in [
            "review/set_chunks",
            "review/update_chunks",
            "review/remove_chunks",
            "review/set_change_briefs",
        ] {
            assert!(!capabilities.contains(&json!(removed)), "{removed}");
        }
    }

    #[test]
    fn removed_curation_methods_return_unknown_method_errors() {
        let (mut server, _dir) = server();
        for method in [
            "review/set_chunks",
            "review/update_chunks",
            "review/remove_chunks",
            "review/set_change_briefs",
        ] {
            let request =
                json!({ "jsonrpc": "2.0", "id": 9, "method": method, "params": {} }).to_string();
            let response = server.handle_line(&request).unwrap();
            assert_eq!(response["error"]["code"], -32000);
            assert_eq!(
                response["error"]["message"],
                format!("unknown method: {method}")
            );
        }
    }

    #[test]
    fn snapshot_present_methods_return_no_live_tui_error() {
        let (mut server, _dir) = server();
        let request = json!({ "jsonrpc": "2.0", "id": 7, "method": "present/status" }).to_string();

        let response = server.handle_line(&request).unwrap();

        assert_eq!(response["id"], 7);
        assert_eq!(response["error"]["code"], -32000);
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("require a live TUI")
        );
    }

    #[test]
    fn read_methods_expose_files_diff_and_comments() {
        let (mut server, _dir) = server();
        server.session.comments[0]
            .replies
            .push(crate::state::CommentReply {
                id: "reply".into(),
                body: "agent reply".into(),
                author: crate::state::Identity::agent(),
                created_at: chrono::DateTime::UNIX_EPOCH,
                result: None,
            });

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
        assert!(comments[0]["session_id"].as_str().is_some());
        assert_eq!(comments[0]["path"], "src/app.rs");
        assert_eq!(
            comments[0]["author"],
            json!({ "kind": "human", "name": "local" })
        );
        assert_eq!(comments[0]["channel"], "note");
        assert_eq!(
            comments[0]["replies"][0]["author"],
            json!({ "kind": "agent", "name": "agent" })
        );
        assert_eq!(
            comments[0]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([
                "action",
                "anchor",
                "author",
                "body",
                "channel",
                "created_at",
                "end_line",
                "id",
                "kind",
                "line",
                "observation",
                "path",
                "replies",
                "session_id",
                "state",
                "updated_at",
            ])
        );
        assert_eq!(
            comments[0]["replies"][0]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from(["author", "body", "created_at", "id"])
        );

        let context = call(&mut server, "review/provenance_context", Value::Null);
        assert_eq!(context["base"], "trunk()");
        assert_eq!(context["revision"], "@");
        assert_eq!(context["files"][0]["path"], "src/app.rs");
        assert!(
            context["files"][0]["raw"]
                .as_str()
                .unwrap()
                .contains("+new")
        );
    }

    #[test]
    fn standalone_read_does_not_clobber_newer_external_review_state() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let snapshot = session(dir.path());
        let mut external = snapshot.to_state();
        external.files.get_mut("src/app.rs").unwrap().viewed = true;
        external.comments[0]
            .replies
            .push(crate::state::CommentReply {
                id: "external-reply".into(),
                body: "new reply".into(),
                author: crate::state::Identity::agent(),
                created_at: chrono::Utc::now(),
                result: None,
            });
        external.comments.push(crate::state::Comment {
            id: "external-comment".into(),
            session_id: external.comments[0].session_id.clone(),
            body: "new external comment".into(),
            ..Default::default()
        });
        external.save(&state_path).unwrap();
        let before = std::fs::read(&state_path).unwrap();
        let mut server = AcpServer::new(snapshot, dir.path().join("agent.json"))
            .unwrap()
            .with_state_path(state_path.clone());
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "review/comments"
        })
        .to_string();

        server
            .serve(std::io::Cursor::new(format!("{request}\n")), Vec::new())
            .unwrap();

        assert_eq!(std::fs::read(&state_path).unwrap(), before);
        let loaded = ReviewState::load_or_default(&state_path).unwrap();
        assert!(loaded.files["src/app.rs"].viewed);
        assert!(
            loaded
                .comments
                .iter()
                .any(|comment| comment.id == "external-comment")
        );
        assert!(
            loaded.comments[0]
                .replies
                .iter()
                .any(|reply| reply.id == "external-reply")
        );
    }

    #[test]
    fn standalone_draft_applies_to_latest_state_and_refreshes_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let snapshot = session(dir.path());
        let mut external = snapshot.to_state();
        external.files.get_mut("src/app.rs").unwrap().viewed = true;
        external.comments.push(crate::state::Comment {
            id: "external-comment".into(),
            session_id: external.comments[0].session_id.clone(),
            body: "keep me".into(),
            ..Default::default()
        });
        external.save(&state_path).unwrap();
        let mut server = AcpServer::new(snapshot, dir.path().join("agent.json"))
            .unwrap()
            .with_state_path(state_path.clone());
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "review/draft_comment",
            "params": { "path": "src/app.rs", "line": 1, "body": "agent draft" }
        })
        .to_string();
        let mut output = Vec::new();

        server
            .serve(std::io::Cursor::new(format!("{request}\n")), &mut output)
            .unwrap();

        let response: Value = serde_json::from_slice(&output).unwrap();
        let draft_id = response["result"]["id"].as_str().unwrap();
        let loaded = ReviewState::load_or_default(&state_path).unwrap();
        assert!(loaded.files["src/app.rs"].viewed);
        assert!(
            loaded
                .comments
                .iter()
                .any(|comment| comment.id == "external-comment")
        );
        assert!(loaded.comments.iter().any(|comment| comment.id == draft_id));
        assert!(
            server
                .session
                .comments
                .iter()
                .any(|comment| comment.id == draft_id)
        );
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
    fn server_appends_durable_agent_drafts_without_overlay_bucket() {
        let (mut server, dir) = server();
        call(
            &mut server,
            "review/draft_comment",
            json!({ "path": "README.md", "body": "first" }),
        );
        call(
            &mut server,
            "review/draft_comment",
            json!({ "path": "README.md", "body": "second" }),
        );

        assert_eq!(server.session.pending_agent_drafts().len(), 2);
        let overlay = AgentOverlay::load_or_default(&dir.path().join("agent.json")).unwrap();
        assert!(!overlay.has_legacy_drafts());
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
        use std::path::Path;

        use super::*;
        use crate::acp::socket::AcpBridge;

        /// Drive the bridge like the TUI event loop until the client's
        /// request has been answered.
        fn pump_until<T>(
            bridge: &mut AcpBridge,
            session: &mut ReviewSession,
            state_path: &Path,
            mut ready: impl FnMut() -> Option<T>,
        ) -> T {
            for _ in 0..200 {
                bridge.process_pending(session, state_path);
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
            state_path: &Path,
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
            let line = pump_until(bridge, session, state_path, || {
                response.lock().unwrap().take()
            });
            reader_thread.join().unwrap();
            serde_json::from_str(&line).unwrap()
        }

        #[test]
        fn socket_serves_live_session_and_applies_writes() {
            let dir = tempfile::tempdir().unwrap();
            let mut session = session(dir.path());
            let socket_path = dir.path().join("acp.sock");
            let overlay_path = dir.path().join("agent.json");
            let state_path = dir.path().join("state.json");
            let mut bridge =
                AcpBridge::bind(socket_path.clone(), overlay_path.clone(), None).unwrap();
            assert!(socket::is_live(&socket_path));

            let mut client = BufReader::new(UnixStream::connect(&socket_path).unwrap());

            let response = call_over_socket(
                &mut bridge,
                &mut session,
                &state_path,
                &mut client,
                &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
            );
            assert_eq!(response["result"]["protocol"], "gander-acp");

            // Live read: session mutations made "in the TUI" are visible.
            session.mark_all_viewed();
            let response = call_over_socket(
                &mut bridge,
                &mut session,
                &state_path,
                &mut client,
                &json!({ "jsonrpc": "2.0", "id": 2, "method": "review/files" }),
            );
            assert_eq!(response["result"][0]["viewed"], true);

            // Write: applied to the live session and persisted to disk.
            let response = call_over_socket(
                &mut bridge,
                &mut session,
                &state_path,
                &mut client,
                &json!({
                    "jsonrpc": "2.0", "id": 3,
                    "method": "review/draft_comment",
                    "params": { "path": "README.md", "body": "typo" },
                }),
            );
            let draft_id = response["result"]["id"].as_str().unwrap();
            assert!(session.comments.iter().any(|draft| draft.id == draft_id
                && draft.author.kind == crate::state::AuthorKind::Agent));
            let persisted = ReviewState::load_or_default(&state_path).unwrap();
            assert!(persisted.comments.iter().any(|draft| draft.id == draft_id));
            let overlay = AgentOverlay::load_or_default(&overlay_path).unwrap();
            assert!(!overlay.has_legacy_drafts());
        }

        #[test]
        fn socket_draft_reports_persistence_failure_without_keeping_volatile_comment() {
            let dir = tempfile::tempdir().unwrap();
            let mut session = session(dir.path());
            let initial_comment_ids = session
                .comments
                .iter()
                .map(|comment| comment.id.clone())
                .collect::<Vec<_>>();
            let socket_path = dir.path().join("acp.sock");
            let overlay_path = dir.path().join("agent.json");
            let state_path = dir.path().join("state.json");
            std::fs::create_dir(state_path.with_extension("json.tmp")).unwrap();
            let mut bridge = AcpBridge::bind(socket_path.clone(), overlay_path, None).unwrap();
            let mut client = BufReader::new(UnixStream::connect(&socket_path).unwrap());

            let response = call_over_socket(
                &mut bridge,
                &mut session,
                &state_path,
                &mut client,
                &json!({
                    "jsonrpc": "2.0", "id": 1,
                    "method": "review/draft_comment",
                    "params": { "path": "README.md", "body": "volatile" },
                }),
            );

            assert_eq!(response["error"]["code"], -32000);
            assert_eq!(
                session
                    .comments
                    .iter()
                    .map(|comment| comment.id.clone())
                    .collect::<Vec<_>>(),
                initial_comment_ids
            );
            assert!(!state_path.exists());
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

    #[test]
    fn configured_agent_identity_stamps_executed_acp_draft_while_writes_persist() {
        let (mut server, dir) = server();
        server.session.agent_identity = crate::state::Identity {
            kind: crate::state::AuthorKind::Agent,
            name: "ACP Bot".into(),
        };

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
        assert!(!overlay.has_legacy_drafts());
        let durable = server
            .session
            .comments
            .iter()
            .find(|comment| comment.id == draft["id"].as_str().unwrap())
            .unwrap();
        assert_eq!(durable.state, crate::state::CommentState::Draft);
        assert_eq!(durable.channel, crate::state::Channel::Onboarding);
        assert_eq!(durable.author.kind, crate::state::AuthorKind::Agent);
        assert_eq!(durable.author.name, "ACP Bot");
    }
}
