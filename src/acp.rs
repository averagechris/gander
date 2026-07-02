//! Agent-collaborative review over ACP.
//!
//! `gander acp` exposes the current review session over a line-delimited
//! JSON-RPC 2.0 protocol on stdio (the transport used by the Agent Client
//! Protocol). Agents can read the diff, comments, and viewed state, and can
//! write review suggestions (ordering, flagged sections, chunks, draft
//! comments) into the shared agent overlay that the TUI surfaces live.
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
        AgentDraft, AgentFlag, AgentOverlay, ChunkPart, DraftState, FlagPriority, ReviewChunk,
    },
    app::ReviewSession,
};

pub const ACP_PROTOCOL_VERSION: u32 = 1;

pub struct AcpServer {
    session: ReviewSession,
    overlay: AgentOverlay,
    overlay_path: PathBuf,
}

impl AcpServer {
    pub fn new(session: ReviewSession, overlay_path: PathBuf) -> Result<Self> {
        let overlay = AgentOverlay::load_or_default(&overlay_path)?;
        Ok(Self {
            session,
            overlay,
            overlay_path,
        })
    }

    /// Serve requests until EOF. One JSON-RPC message per line.
    pub fn serve(&mut self, input: impl BufRead, mut output: impl Write) -> Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(response) = self.handle_line(&line) {
                serde_json::to_writer(&mut output, &response)?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
        }
        Ok(())
    }

    /// Handle one raw JSON-RPC message; `None` means no response is due
    /// (notification).
    pub fn handle_line(&mut self, line: &str) -> Option<Value> {
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

        let result = self.dispatch(&method, &params);
        let id = id?;
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => error_response(id, -32000, &message),
        })
    }

    fn dispatch(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        match method {
            "initialize" => Ok(json!({
                "protocol": "gander-acp",
                "version": ACP_PROTOCOL_VERSION,
                "capabilities": [
                    "review/summary",
                    "review/files",
                    "review/file_diff",
                    "review/comments",
                    "review/overlay",
                    "review/set_ordering",
                    "review/flag_section",
                    "review/set_chunks",
                    "review/draft_comment",
                ],
            })),
            "review/summary" => Ok(json!({
                "repo": self.session.repo.display().to_string(),
                "base": self.session.target.base,
                "revision": self.session.target.rev,
                "summary": self.session.summary_line(),
            })),
            "review/files" => Ok(json!(
                self.session
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
                let file = self
                    .session
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
                self.session
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
                    .filter(|path| self.session.files.iter().all(|file| &file.path != *path))
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
                self.overlay.chunks = chunks;
                self.save_overlay()?;
                Ok(json!({ "chunks": self.overlay.chunks.len() }))
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
        rationale: value
            .get("rationale")
            .and_then(Value::as_str)
            .map(str::to_owned),
        parts,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{diff::DiffSet, jj::ReviewTarget, state::ReviewState};

    fn server() -> (AcpServer, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
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
            dir.path().to_path_buf(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.add_comment("file note".into());
        let overlay_path = AgentOverlay::default_path(dir.path());
        (AcpServer::new(session, overlay_path).unwrap(), dir)
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

        let overlay =
            AgentOverlay::load_or_default(&AgentOverlay::default_path(dir.path())).unwrap();
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
        let overlay_path = AgentOverlay::default_path(dir.path());
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
}
