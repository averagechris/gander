//! Standalone loopback web peer.
//!
//! The browser is a renderer over the app-owned reading projection. This
//! durable state and jj changes are projected into surgical SSE patches.
//! The browser also reports ephemeral interaction state and renders the same
//! socket-driven presentation commands as the TUI. Durable browser mutations
//! are thin, generation-guarded adapters over the shared review services.

use std::{
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
    task::{Context as TaskContext, Poll},
    time::{Duration, SystemTime},
};

use axum::{
    Router,
    extract::{Json, Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{
        Html, IntoResponse, Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{get, post},
};
use chrono::Utc;
use color_eyre::eyre::{Context, Result};
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpListener;

use crate::{
    acp::socket::{AcpBridge, PresentCommand},
    app::{Focus, ReadingRegion, ReadingRegionKind, ReviewSession},
    attention::{self, SkimSelection},
    config::ThemeConfig,
    diff::{DiffSet, FileDiff},
    generated::GeneratedMatcher,
    jj::JjBackend,
    registry::{InstanceInfo, InstanceRegistration},
    review,
    state::{
        ActionIntent, AuthorKind, Channel, CommentKind, CommentState, ReviewState,
        ReviewStateTombstones, Salience,
    },
    web_render::{
        self, COMPONENT_CSS, GuideView, PREPAINT_SCRIPT, RenderMode, RenderOptions,
        THEME_CONTROL_SCRIPT,
    },
};

const COMPONENT_JS: &str = include_str!("web.js");
const INITIAL_REGION_WINDOW: usize = 4;
const WATCH_TICK: Duration = Duration::from_millis(250);
const REPO_POLL_INTERVAL: Duration = Duration::from_secs(2);
const SSE_KEEPALIVE: Duration = Duration::from_secs(15);
const SSE_BROADCAST_CAPACITY: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpotlightIdentity {
    step_id: String,
    part: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WebPresentationState {
    identity: SpotlightIdentity,
    index: usize,
    stale: bool,
}

#[derive(Debug, Clone)]
struct PresentEvent {
    sequence: u64,
    command: &'static str,
    status: Value,
    target: Option<PresentTarget>,
    note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PresentTarget {
    path: String,
    line: Option<usize>,
    end_line: Option<usize>,
    region_id: Option<String>,
    row_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct WebInteractions {
    next_sequence: u64,
    next_connection: u64,
    tabs: std::collections::BTreeMap<String, TabInteraction>,
}

#[derive(Debug, Clone)]
struct TabInteraction {
    sequence: u64,
    connection: u64,
    connected: bool,
    focus: Option<BrowserFocus>,
    busy: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct BrowserFocus {
    path: Option<String>,
    old_line: Option<usize>,
    new_line: Option<usize>,
    hunk_header: Option<String>,
    pane: String,
}

#[derive(Debug, Deserialize)]
struct InteractionReport {
    tab_id: String,
    #[serde(default)]
    focus: Option<BrowserFocus>,
    #[serde(default)]
    busy: Option<String>,
}

impl WebInteractions {
    fn connect(&mut self, tab_id: &str) -> Option<u64> {
        if !valid_tab_id(tab_id) {
            return None;
        }
        self.next_connection = self.next_connection.saturating_add(1);
        let connection = self.next_connection;
        self.tabs
            .entry(tab_id.to_owned())
            .and_modify(|tab| {
                tab.connection = connection;
                tab.connected = true;
            })
            .or_insert(TabInteraction {
                sequence: 0,
                connection,
                connected: true,
                focus: None,
                busy: None,
            });
        Some(connection)
    }

    fn disconnect(&mut self, tab_id: &str, connection: u64) {
        if self
            .tabs
            .get(tab_id)
            .is_some_and(|tab| tab.connection == connection)
        {
            self.tabs.remove(tab_id);
        }
    }

    fn report(&mut self, report: InteractionReport) -> Result<(), &'static str> {
        if !valid_tab_id(&report.tab_id) {
            return Err("invalid tab_id");
        }
        if report
            .busy
            .as_deref()
            .is_some_and(|mode| !matches!(mode, "search" | "dialog" | "comment editor"))
        {
            return Err("invalid busy mode");
        }
        if report
            .focus
            .as_ref()
            .is_some_and(|focus| !matches!(focus.pane.as_str(), "files" | "diff"))
        {
            return Err("invalid focus pane");
        }
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.tabs
            .entry(report.tab_id)
            .and_modify(|tab| {
                tab.sequence = self.next_sequence;
                tab.connected = true;
                tab.focus.clone_from(&report.focus);
                tab.busy.clone_from(&report.busy);
            })
            .or_insert(TabInteraction {
                sequence: self.next_sequence,
                connection: 0,
                connected: true,
                focus: report.focus,
                busy: report.busy,
            });
        Ok(())
    }

    /// Deterministic controlling-tab arbitration: greatest server-observed
    /// input sequence wins, with tab id as an explicit tie-breaker.
    fn controlling(&self) -> Option<(&str, &TabInteraction)> {
        self.tabs
            .iter()
            .filter(|(_, tab)| tab.connected)
            .max_by_key(|(id, tab)| (tab.sequence, id.as_str()))
            .map(|(id, tab)| (id.as_str(), tab))
    }
}

fn valid_tab_id(tab_id: &str) -> bool {
    !tab_id.is_empty()
        && tab_id.len() <= 128
        && tab_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(crate) struct WebParams {
    pub session: ReviewSession,
    pub overlay_path: PathBuf,
    pub state_path: PathBuf,
    pub socket_path: PathBuf,
    pub registry_dir: PathBuf,
    pub workspace_root: PathBuf,
    pub acp_jj: Box<dyn JjBackend + Send>,
    pub watch_jj: Option<Box<dyn JjBackend + Send>>,
    pub ignore_globs: Vec<String>,
    pub generated_matcher: GeneratedMatcher,
    pub port: u16,
    pub no_open: bool,
    pub theme: ThemeConfig,
    pub extra_css: Option<PathBuf>,
}

#[derive(Clone)]
struct HttpState {
    token: Arc<str>,
    expected_host: Arc<str>,
    expected_origin: Arc<str>,
    summary: Arc<str>,
    base: Arc<str>,
    rev: Arc<str>,
    target: Arc<str>,
    theme_css: Arc<str>,
    review: Arc<RwLock<WebReview>>,
    events: tokio::sync::broadcast::Sender<Arc<ProjectionEvent>>,
    present_events: tokio::sync::watch::Receiver<Option<Arc<PresentEvent>>>,
    interactions: Arc<Mutex<WebInteractions>>,
    shutdown: tokio::sync::watch::Receiver<bool>,
    extra_css: Option<Arc<str>>,
    registration: Arc<Mutex<InstanceRegistration>>,
    actions: tokio::sync::mpsc::Sender<ActionEnvelope>,
}

#[derive(Debug)]
struct ActionEnvelope {
    expected_generation: u64,
    command: ActionCommand,
    response: tokio::sync::oneshot::Sender<std::result::Result<ActionResult, ActionError>>,
}

#[derive(Debug)]
enum ActionCommand {
    FileViewed {
        path: String,
        viewed: bool,
    },
    Acknowledge {
        selection: SkimSelection,
    },
    CommentAdd(CommentAddAction),
    CommentEdit(CommentEditAction),
    CommentReply(CommentReplyAction),
    CommentState {
        id: String,
        state: CommentState,
    },
    DraftAccept {
        id: String,
        body: Option<String>,
        channel: Option<Channel>,
    },
    DraftDiscard {
        id: String,
    },
    Salience {
        verb: SalienceVerb,
        target: TargetAction,
        salience: Option<Salience>,
        rationale: Option<String>,
    },
    Walkthrough {
        verb: WalkthroughVerb,
        step_id: Option<String>,
        part: Option<usize>,
    },
}

#[derive(Debug, Clone, Copy)]
enum SalienceVerb {
    Set,
    Clear,
    Promote,
    Demote,
}

#[derive(Debug, Clone, Copy)]
enum WalkthroughVerb {
    Next,
    Prev,
    Goto,
}

#[derive(Debug, Serialize)]
struct ActionResult {
    generation: u64,
    result: Value,
}

#[derive(Debug)]
struct ActionError {
    status: StatusCode,
    message: String,
}

impl ActionError {
    fn bad(error: impl std::fmt::Display) -> Self {
        let message = error.to_string();
        let status = if message.starts_with("unknown ") || message.contains("not found") {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        };
        Self { status, message }
    }
    fn conflict(current: u64) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: format!("stale action generation; current generation is {current}"),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationAction {
    expected_generation: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileViewedAction {
    expected_generation: u64,
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcknowledgeAction {
    expected_generation: u64,
    #[serde(default)]
    fold_id: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetAction {
    path: String,
    #[serde(default)]
    line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentAddAction {
    expected_generation: u64,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
    body: String,
    #[serde(default)]
    kind: Option<CommentKind>,
    #[serde(default)]
    action: Option<ActionIntent>,
    #[serde(default)]
    state: Option<CommentState>,
    #[serde(default)]
    channel: Option<Channel>,
    #[serde(default)]
    source_comment_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentEditAction {
    expected_generation: u64,
    id: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_patch")]
    kind: Option<Option<CommentKind>>,
    #[serde(default, deserialize_with = "deserialize_optional_patch")]
    action: Option<Option<ActionIntent>>,
    #[serde(default)]
    channel: Option<Channel>,
}

fn deserialize_optional_patch<'de, D, T>(
    deserializer: D,
) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentReplyAction {
    expected_generation: u64,
    id: String,
    body: String,
    #[serde(default)]
    resolve: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentStateAction {
    expected_generation: u64,
    id: String,
    state: CommentState,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftAcceptAction {
    expected_generation: u64,
    id: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    channel: Option<Channel>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdAction {
    expected_generation: u64,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SalienceAction {
    expected_generation: u64,
    target: TargetAction,
    #[serde(default)]
    salience: Option<Salience>,
    #[serde(default)]
    rationale: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WalkthroughGotoAction {
    expected_generation: u64,
    step_id: String,
    #[serde(default)]
    part: Option<usize>,
}

type WebReview = GuideView;

#[derive(Debug, Clone)]
struct RegionPatch {
    id: String,
    guided: Option<String>,
    full: Option<String>,
    remove: bool,
}

#[derive(Debug, Clone)]
struct ProjectionEvent {
    generation: u64,
    full: bool,
    patches: Vec<RegionPatch>,
    order: Vec<String>,
}

struct ReceiverStream<T> {
    receiver: tokio::sync::mpsc::Receiver<T>,
}

impl<T> Stream for ReceiverStream<T> {
    type Item = std::result::Result<T, std::convert::Infallible>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx).map(|item| item.map(Ok))
    }
}

struct WebWatcher {
    state_path: PathBuf,
    overlay_path: PathBuf,
    state_mtime: Option<SystemTime>,
    overlay_mtime: Option<SystemTime>,
    repo_fingerprint: Option<String>,
    last_repo_poll: Option<std::time::Instant>,
    jj: Box<dyn JjBackend + Send>,
    ignore_globs: Vec<String>,
    generated_matcher: GeneratedMatcher,
}

impl WebWatcher {
    fn new(params: &mut WebParams) -> Self {
        Self {
            state_path: params.state_path.clone(),
            overlay_path: params.overlay_path.clone(),
            state_mtime: file_mtime(&params.state_path),
            overlay_mtime: file_mtime(&params.overlay_path),
            repo_fingerprint: None,
            last_repo_poll: None,
            jj: params
                .watch_jj
                .take()
                .expect("web watcher backend is present"),
            ignore_globs: params.ignore_globs.clone(),
            generated_matcher: params.generated_matcher.clone(),
        }
    }

    fn poll_files(&mut self, session: &mut ReviewSession, baseline: &mut ReviewState) {
        let state_mtime = file_mtime(&self.state_path);
        if state_mtime.is_some()
            && state_mtime != self.state_mtime
            && let Ok(external) = ReviewState::load_or_default(&self.state_path)
        {
            let merged = ReviewState::merge_changes_since(
                external,
                baseline,
                session.to_state(),
                &ReviewStateTombstones::default(),
            );
            session.apply_review_state(merged.clone());
            *baseline = merged;
            self.state_mtime = state_mtime;
        }

        let overlay_mtime = file_mtime(&self.overlay_path);
        if overlay_mtime.is_some()
            && overlay_mtime != self.overlay_mtime
            && let Ok(overlay) = crate::agent::AgentOverlay::load_or_default(&self.overlay_path)
        {
            session.apply_agent_overlay(&overlay);
            // Overlay ordering/flags feed the stream but predate explicit
            // generation invalidation at this seam.
            session.touch_stream_inputs();
            self.overlay_mtime = overlay_mtime;
        }
    }

    fn poll_repo(&mut self, session: &mut ReviewSession) {
        let now = std::time::Instant::now();
        if self
            .last_repo_poll
            .is_some_and(|last| now.duration_since(last) < REPO_POLL_INTERVAL)
        {
            return;
        }
        self.last_repo_poll = Some(now);
        // Exactly one deliberate snapshot per repo poll. Every operation after
        // this call is implemented by JjBackend's --ignore-working-copy reads.
        if self.jj.snapshot_working_copy(&session.repo).is_err() {
            return;
        }
        let Ok(fingerprint) = self.jj.change_fingerprint(&session.repo, &session.target) else {
            return;
        };
        let changed = self
            .repo_fingerprint
            .as_ref()
            .is_some_and(|previous| previous != &fingerprint);
        self.repo_fingerprint = Some(fingerprint);
        if changed {
            let _ = self.reload_target(session);
        }
    }

    fn reload_local_state(
        &mut self,
        session: &mut ReviewSession,
        baseline: &mut ReviewState,
    ) -> Result<()> {
        let external = ReviewState::load_or_default(&self.state_path)?;
        let merged = ReviewState::merge_changes_since(
            external,
            baseline,
            session.to_state(),
            &ReviewStateTombstones::default(),
        );
        session.apply_review_state(merged.clone());
        *baseline = merged;
        self.state_mtime = file_mtime(&self.state_path);
        let overlay = crate::agent::AgentOverlay::load_or_default(&self.overlay_path)?;
        session.apply_agent_overlay(&overlay);
        session.touch_stream_inputs();
        self.overlay_mtime = file_mtime(&self.overlay_path);
        Ok(())
    }

    fn reload_target(&self, session: &mut ReviewSession) -> Result<()> {
        let target = session.target.clone();
        let raw = self.jj.diff(&session.repo, &target)?;
        let mut diff = DiffSet::parse(&raw)?;
        diff.apply_ignores(&self.ignore_globs)?;
        session.replace_diff_preserving_view(target.clone(), diff);
        session.set_target_author(
            self.jj
                .target_author(&session.repo, &target)
                .unwrap_or_default(),
        );
        session.annotate_generated_where(|file| {
            self.generated_matcher.is_match(&file.path)
                || crate::generated::diff_content_looks_generated(&file.diff)
        });
        reload_chapter_metadata(self.jj.as_ref(), session);
        if let Ok(overlay) = crate::agent::AgentOverlay::load_or_default(&self.overlay_path) {
            session.apply_agent_overlay(&overlay);
            session.touch_stream_inputs();
        }
        Ok(())
    }
}

fn file_mtime(path: &std::path::Path) -> Option<SystemTime> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn reload_chapter_metadata(jj: &dyn JjBackend, session: &mut ReviewSession) {
    let stack = jj
        .stack_changes(&session.repo, &session.target)
        .unwrap_or_default();
    session.stack_changes = stack.clone();
    session.change_diffs.clear();
    for change in stack {
        let target = crate::jj::ReviewTarget::new(
            format!("{}-", change.change_id),
            change.change_id.clone(),
        );
        if let Ok(raw) = jj.diff(&session.repo, &target)
            && let Ok(diff) = DiffSet::parse(&raw)
        {
            session.change_diffs.push((change.change_id, diff));
        }
    }
    session.touch_stream_inputs();
}

fn publish_projection(state: &HttpState, session: &ReviewSession) {
    // Projection and rendering happen before the short write-lock section.
    // HTTP handlers therefore never wait on jj reads or stream materialization.
    let mut next = WebReview::from_session(session);
    let current = match state.review.read() {
        Ok(current) => current.clone(),
        Err(_) => return,
    };
    next.generation = current.generation;
    if diff_projection(&current, &next).patches.is_empty() {
        return;
    }
    next.generation = current.generation.saturating_add(1);
    let event = diff_projection(&current, &next);
    let Ok(mut stored) = state.review.write() else {
        return;
    };
    *stored = next;
    drop(stored);
    let _ = state.events.send(Arc::new(event));
}

fn session_files(session: &ReviewSession) -> Vec<FileDiff> {
    session.files.iter().map(|file| file.diff.clone()).collect()
}

fn active_state_session_index(state: &mut ReviewState, session: &ReviewSession) -> usize {
    let spec = review::SessionTargetSpec {
        repo: Some(review::canonical_repo_identity(&session.repo)),
        base: Some(session.target.base.clone()),
        revision: Some(session.target.rev.clone()),
        revset: Some(session.target.to_string()),
    };
    let id = review::ensure_session(state, &spec, None).id.clone();
    state
        .sessions
        .iter()
        .position(|candidate| candidate.id == id)
        .expect("ensured session exists")
}

fn inferred_web_channel(
    session: &ReviewSession,
    state: &ReviewState,
    session_id: &str,
    onboarding_target: bool,
) -> Channel {
    let agent_attached = state.comments.iter().any(|comment| {
        comment.belongs_to_session(session_id) && comment.author.kind == AuthorKind::Agent
    });
    review::infer_comment_channel(review::ChannelInferenceContext {
        onboarding_target,
        agent_attached,
        configured_human_name: session.configured_human_name.as_deref(),
        configured_human_email: session.configured_human_email.as_deref(),
        target_author_name: session.target_author_name.as_deref(),
        target_author_email: session.target_author_email.as_deref(),
        fixed_default: session.comment_default_channel,
        ..review::ChannelInferenceContext::default()
    })
}

fn process_action(
    expected_generation: u64,
    command: ActionCommand,
    session: &mut ReviewSession,
    state_path: &std::path::Path,
    baseline: &mut ReviewState,
    watcher: &mut WebWatcher,
    http: &HttpState,
) -> std::result::Result<ActionResult, ActionError> {
    let current = http
        .review
        .read()
        .map_err(|_| ActionError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "review projection unavailable".into(),
        })?
        .generation;
    if expected_generation != current {
        return Err(ActionError::conflict(current));
    }

    let files = session_files(session);
    let result: Value;
    match command {
        ActionCommand::Walkthrough {
            verb,
            step_id,
            part,
        } => {
            let destination = match verb {
                WalkthroughVerb::Next => session.jump_spotlight_with_identity(1),
                WalkthroughVerb::Prev => session.jump_spotlight_with_identity(-1),
                WalkthroughVerb::Goto => {
                    let step_id = step_id.expect("goto has step id");
                    let index = if let Some(part) = part {
                        session.spotlight_index_for_identity(&step_id, part)
                    } else {
                        session.spotlight_index_for_step(&step_id)
                    }
                    .ok_or_else(|| {
                        ActionError::bad(format!("unknown current walkthrough step `{step_id}`"))
                    })?;
                    session.jump_to_spotlight_index(index)
                }
            }
            .ok_or_else(|| ActionError::bad("no current walkthrough spotlight"))?;
            result = json!({"step_id": destination.0, "part": destination.1});
        }
        command => {
            let mut state = session.to_state();
            let session_index = active_state_session_index(&mut state, session);
            match command {
                ActionCommand::FileViewed { path, viewed } => {
                    let file = files.iter().find(|file| file.path == path).ok_or_else(|| {
                        ActionError::bad(format!("unknown current file `{path}`"))
                    })?;
                    review::set_file_viewed(&mut state, file, viewed);
                    result =
                        json!({"path": path, "viewed": viewed, "fingerprint": file.fingerprint});
                }
                ActionCommand::Acknowledge { selection } => {
                    let outcome = attention::acknowledge_skim_folds(
                        &mut state.sessions[session_index],
                        &files,
                        &selection,
                    )
                    .map_err(ActionError::bad)?;
                    attention::apply_whole_file_viewed_effects(
                        &mut state,
                        &files,
                        &outcome.whole_files_viewed,
                    );
                    result = serde_json::to_value(outcome).expect("acknowledgement serializes");
                }
                ActionCommand::CommentAdd(action) => {
                    let source_id = action
                        .source_comment_id
                        .as_deref()
                        .map(|source| review::resolve_comment_id(&state.comments, source))
                        .transpose()
                        .map_err(ActionError::bad)?;
                    let source = source_id.as_deref().map(|source_id| {
                        state
                            .comments
                            .iter()
                            .find(|comment| comment.id == source_id)
                            .expect("resolved source exists")
                            .clone()
                    });
                    if let Some(source) = &source {
                        if !source.belongs_to_session(&state.sessions[session_index].id) {
                            return Err(ActionError::bad(
                                "source comment belongs to another review session",
                            ));
                        }
                        if source.author.kind != AuthorKind::Agent
                            || source.channel != Channel::Onboarding
                        {
                            return Err(ActionError::bad(
                                "source comment is not an agent onboarding annotation",
                            ));
                        }
                    }
                    let path = source
                        .as_ref()
                        .and_then(|source| source.path.clone())
                        .or(action.path);
                    let line = source
                        .as_ref()
                        .and_then(|source| source.line)
                        .or(action.line);
                    let end_line = source
                        .as_ref()
                        .and_then(|source| source.end_line)
                        .or(action.end_line);
                    if path.is_none() && (line.is_some() || end_line.is_some()) {
                        return Err(ActionError::bad("comment line requires path"));
                    }
                    if line.is_none() && end_line.is_some() {
                        return Err(ActionError::bad("comment end_line requires line"));
                    }
                    let anchor = match source.as_ref().and_then(|source| source.anchor.clone()) {
                        Some(anchor) => Some(anchor),
                        None => match path.as_deref() {
                            Some(path) => {
                                let file = files.iter().find(|file| file.path == path).ok_or_else(
                                    || ActionError::bad(format!("unknown current file `{path}`")),
                                )?;
                                crate::anchor::comment_anchor_for_file_diff(file, line, end_line)
                            }
                            None => None,
                        },
                    };
                    let onboarding_target = source.is_some();
                    let channel = action.channel.unwrap_or_else(|| {
                        inferred_web_channel(
                            session,
                            &state,
                            &state.sessions[session_index].id,
                            onboarding_target,
                        )
                    });
                    let requested_state = action.state.unwrap_or(session.comment_initial_state);
                    let comment_state =
                        if requested_state == CommentState::Todo && !channel.permits_todo() {
                            CommentState::Draft
                        } else {
                            requested_state
                        };
                    let snapshot = crate::provenance::SnapshotEvidence::capture(
                        Utc::now(),
                        state.sessions[session_index].id.clone(),
                        state.sessions[session_index].target.clone(),
                        files.iter(),
                    );
                    let observation =
                        crate::provenance::CommentObservation::new(snapshot, anchor.clone());
                    let new = review::NewComment {
                        session_id: state.sessions[session_index].id.clone(),
                        path,
                        line,
                        end_line,
                        anchor,
                        observation: Some(observation),
                        body: action.body,
                        kind: action.kind,
                        action: action.action,
                        state: comment_state,
                        author: session.human_identity.clone(),
                        channel,
                    };
                    let comment = review::add_comment(
                        &mut state.sessions[session_index],
                        &mut state.comments,
                        new,
                    )
                    .map_err(ActionError::bad)?;
                    if let Some(source_id) = source_id {
                        state
                            .comments
                            .iter_mut()
                            .find(|saved| saved.id == comment.id)
                            .expect("new comment exists")
                            .source_comment_id = Some(source_id);
                    }
                    result = serde_json::to_value(
                        state
                            .comments
                            .iter()
                            .find(|saved| saved.id == comment.id)
                            .expect("new comment exists"),
                    )
                    .expect("comment serializes");
                }
                ActionCommand::CommentEdit(action) => {
                    let comment = review::edit_comment(
                        &mut state.sessions[session_index],
                        &mut state.comments,
                        &action.id,
                        review::CommentEdits {
                            body: action.body,
                            kind: action.kind,
                            action: action.action,
                            channel: action.channel,
                            ..Default::default()
                        },
                    )
                    .map_err(ActionError::bad)?;
                    result = serde_json::to_value(comment).expect("comment serializes");
                }
                ActionCommand::CommentReply(action) => {
                    let snapshot = crate::provenance::SnapshotEvidence::capture(
                        Utc::now(),
                        state.sessions[session_index].id.clone(),
                        state.sessions[session_index].target.clone(),
                        files.iter(),
                    );
                    let comment = review::reply_and_maybe_resolve_comment(
                        &mut state.sessions[session_index],
                        &mut state.comments,
                        &action.id,
                        action.body,
                        session.human_identity.clone(),
                        action.resolve,
                        snapshot,
                    )
                    .map_err(ActionError::bad)?;
                    result = serde_json::to_value(comment).expect("comment serializes");
                }
                ActionCommand::CommentState { id, state: next } => {
                    let comment = review::set_comment_state(
                        &mut state.sessions[session_index],
                        &mut state.comments,
                        &id,
                        next,
                    )
                    .map_err(ActionError::bad)?;
                    result = serde_json::to_value(comment).expect("comment serializes");
                }
                ActionCommand::DraftAccept { id, body, channel } => {
                    let inferred = channel.unwrap_or_else(|| {
                        inferred_web_channel(
                            session,
                            &state,
                            &state.sessions[session_index].id,
                            true,
                        )
                    });
                    let comment = review::accept_agent_draft(
                        &mut state.sessions[session_index],
                        &mut state.comments,
                        &id,
                        body,
                        inferred,
                    )
                    .map_err(ActionError::bad)?;
                    result = serde_json::to_value(comment).expect("comment serializes");
                }
                ActionCommand::DraftDiscard { id } => {
                    let comment = review::discard_agent_draft(
                        &mut state.sessions[session_index],
                        &mut state.comments,
                        &id,
                    )
                    .map_err(ActionError::bad)?;
                    result = json!({"discarded": comment.id});
                }
                ActionCommand::Salience {
                    verb,
                    target,
                    salience,
                    rationale,
                } => {
                    let file = files
                        .iter()
                        .find(|file| file.path == target.path)
                        .ok_or_else(|| {
                            ActionError::bad(format!("unknown current file `{}`", target.path))
                        })?;
                    let durable_target = attention::target_for_file_diff(
                        file,
                        &target.path,
                        target.line,
                        target.end_line,
                    )
                    .map_err(ActionError::bad)?;
                    result = match verb {
                        SalienceVerb::Clear => {
                            json!({"cleared": attention::clear_human_attention(&mut state.sessions[session_index], &durable_target)})
                        }
                        SalienceVerb::Set => serde_json::to_value(
                            attention::set_human_attention(
                                &mut state.sessions[session_index],
                                durable_target,
                                salience.ok_or_else(|| {
                                    ActionError::bad("salience-set requires salience")
                                })?,
                                rationale,
                            )
                            .map_err(ActionError::bad)?,
                        )
                        .expect("attention serializes"),
                        SalienceVerb::Promote => serde_json::to_value(
                            attention::promote_human_attention(
                                &mut state.sessions[session_index],
                                durable_target,
                                rationale,
                                &files,
                            )
                            .map_err(ActionError::bad)?,
                        )
                        .expect("attention serializes"),
                        SalienceVerb::Demote => serde_json::to_value(
                            attention::demote_human_attention(
                                &mut state.sessions[session_index],
                                durable_target,
                                rationale,
                                &files,
                            )
                            .map_err(ActionError::bad)?,
                        )
                        .expect("attention serializes"),
                    };
                }
                ActionCommand::Walkthrough { .. } => unreachable!(),
            }
            session.apply_review_state(state);
        }
    }

    let local = session.to_state();
    let merged = review::merge_live_state_file(
        state_path,
        baseline,
        local,
        &ReviewStateTombstones::default(),
    )
    .map_err(|error| ActionError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to save review action: {error}"),
    })?;
    session.apply_review_state(merged.clone());
    *baseline = merged;
    watcher.state_mtime = file_mtime(state_path);
    publish_projection(http, session);
    let generation = http
        .review
        .read()
        .map_err(|_| ActionError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "review projection unavailable".into(),
        })?
        .generation;
    Ok(ActionResult { generation, result })
}

fn diff_projection(previous: &WebReview, next: &WebReview) -> ProjectionEvent {
    let previous_regions = rendered_projection(previous);
    let next_regions = rendered_projection(next);
    let mut patches = Vec::new();
    for (id, (guided, full)) in &next_regions {
        if previous_regions.get(id) != Some(&(guided.clone(), full.clone())) {
            patches.push(RegionPatch {
                id: id.clone(),
                guided: Some(guided.clone()),
                full: Some(full.clone()),
                remove: false,
            });
        }
    }
    for id in previous_regions.keys() {
        if !next_regions.contains_key(id) {
            patches.push(RegionPatch {
                id: id.clone(),
                guided: None,
                full: None,
                remove: true,
            });
        }
    }
    ProjectionEvent {
        generation: next.generation,
        full: false,
        patches,
        order: next
            .projection
            .regions
            .iter()
            .map(|region| region.id.clone())
            .collect(),
    }
}

fn rendered_projection(review: &WebReview) -> std::collections::BTreeMap<String, (String, String)> {
    let mut regions = std::collections::BTreeMap::new();
    regions.insert(
        "overview".into(),
        (render_overview(review), render_overview(review)),
    );
    regions.insert(
        "coverage".into(),
        (render_coverage(review), render_coverage(review)),
    );
    regions.insert(
        "file-tree".into(),
        (render_file_tree_html(review), render_file_tree_html(review)),
    );
    regions.insert(
        "footer".into(),
        (render_footer(review), render_footer(review)),
    );
    for region in &review.projection.regions {
        regions.insert(
            region.id.clone(),
            (
                render_region(region, true, RenderMode::Guided),
                render_region(region, true, RenderMode::Full),
            ),
        );
    }
    regions
}

fn full_projection_event(state: &HttpState) -> Arc<ProjectionEvent> {
    let review = state
        .review
        .read()
        .expect("web projection lock poisoned")
        .clone();
    let patches = rendered_projection(&review)
        .into_iter()
        .map(|(id, (guided, full))| RegionPatch {
            id,
            guided: Some(guided),
            full: Some(full),
            remove: false,
        })
        .collect();
    Arc::new(ProjectionEvent {
        generation: review.generation,
        full: true,
        patches,
        order: review
            .projection
            .regions
            .iter()
            .map(|region| region.id.clone())
            .collect(),
    })
}

fn sse_state_event(event: &ProjectionEvent) -> SseEvent {
    let patches = event
        .patches
        .iter()
        .map(|patch| {
            json!({
                "id": patch.id,
                "guided": patch.guided,
                "full": patch.full,
                "remove": patch.remove,
            })
        })
        .collect::<Vec<_>>();
    SseEvent::default()
        .event("state")
        .id(event.generation.to_string())
        .json_data(json!({
            "generation": event.generation,
            "full": event.full,
            "patches": patches,
            "order": event.order,
        }))
        .expect("projection event is JSON serializable")
}

fn sse_present_event(event: &PresentEvent) -> SseEvent {
    SseEvent::default()
        .event("present")
        .json_data(json!({
            "sequence": event.sequence,
            "command": event.command,
            "status": event.status,
            "target": event.target,
            "note": event.note,
        }))
        .expect("presentation event is JSON serializable")
}

pub(crate) fn run(params: WebParams) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .wrap_err("failed to start the web runtime")?;
    runtime.block_on(run_async(params))
}

async fn run_async(mut params: WebParams) -> Result<()> {
    let listener = TcpListener::bind(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        params.port,
    ))
    .await
    .with_context(|| format!("failed to bind 127.0.0.1:{}", params.port))?;
    let address = listener.local_addr()?;
    debug_assert_eq!(address.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    let mut watcher = WebWatcher::new(&mut params);

    let mut bridge = AcpBridge::bind(
        params.socket_path.clone(),
        params.overlay_path,
        Some(params.acp_jj),
    )?;
    let now = Utc::now();
    let registration = InstanceRegistration::register(
        &params.registry_dir,
        InstanceInfo {
            pid: std::process::id(),
            workspace_root: params.workspace_root,
            base: params.session.target.base.clone(),
            rev: params.session.target.rev.clone(),
            summary: params.session.summary_line(),
            socket_path: params.socket_path,
            started_at: now,
            last_input_at: now,
        },
    )?;

    let initial_review = WebReview::from_session(&params.session);
    let review = Arc::new(RwLock::new(initial_review));
    let (events, _) = tokio::sync::broadcast::channel(SSE_BROADCAST_CAPACITY);
    // A watch channel deliberately retains only the newest presenter move.
    // Fast agent driving therefore cannot queue a browser scroll storm.
    let (present_tx, present_rx) = tokio::sync::watch::channel(None);
    let (action_tx, mut action_rx) = tokio::sync::mpsc::channel(16);
    let (stream_shutdown_tx, stream_shutdown_rx) = tokio::sync::watch::channel(false);
    let token = uuid::Uuid::new_v4().to_string();
    let host = format!("127.0.0.1:{}", address.port());
    let origin = format!("http://{host}");
    let url = format!("{origin}/?token={token}");
    let http_state = HttpState {
        token: Arc::from(token),
        expected_host: Arc::from(host),
        expected_origin: Arc::from(origin),
        summary: Arc::from(params.session.summary_line()),
        base: Arc::from(params.session.target.base.clone()),
        rev: Arc::from(params.session.target.rev.clone()),
        target: Arc::from(params.session.target.to_string()),
        theme_css: Arc::from(render_theme_css(&params.theme)),
        review,
        events,
        present_events: present_rx,
        interactions: Arc::new(Mutex::new(WebInteractions::default())),
        shutdown: stream_shutdown_rx,
        extra_css: params
            .extra_css
            .as_deref()
            .map(read_extra_css)
            .transpose()?
            .map(Arc::from),
        registration: Arc::new(Mutex::new(registration)),
        actions: action_tx,
    };
    let app = router(http_state.clone());

    println!("{url}");
    if !params.no_open {
        eprintln!(
            "gander web: browser auto-open is unavailable; open the printed loopback URL manually"
        );
    }

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        let _ = stream_shutdown_tx.send(true);
        let _ = shutdown_tx.send(());
    });
    let server = async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
    };
    tokio::pin!(server);

    let mut baseline = params.session.to_state();
    let mut presentation = None;
    let mut present_sequence = 0u64;
    let mut tick = tokio::time::interval(WATCH_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = &mut server => {
                result.wrap_err("web server failed")?;
                break;
            }
            _ = tick.tick() => {
                let before = params.session.stream_inputs_generation();
                process_acp_requests(
                    &mut bridge,
                    &mut params.session,
                    &params.state_path,
                    &mut baseline,
                    &mut watcher,
                    &mut presentation,
                    &mut present_sequence,
                    &present_tx,
                    &http_state,
                );
                watcher.poll_files(&mut params.session, &mut baseline);
                watcher.poll_repo(&mut params.session);
                if reconcile_web_presentation(&params.session, &mut presentation) {
                    publish_present_event(
                        &params.session,
                        &mut present_sequence,
                        &present_tx,
                        "sync",
                        &presentation,
                        None,
                        None,
                    );
                }
                if params.session.stream_inputs_generation() != before {
                    publish_projection(&http_state, &params.session);
                }
            }
            Some(action) = action_rx.recv() => {
                let result = process_action(
                    action.expected_generation,
                    action.command,
                    &mut params.session,
                    &params.state_path,
                    &mut baseline,
                    &mut watcher,
                    &http_state,
                );
                let _ = action.response.send(result);
            }
        }
    }

    // Keep the registration alive until after the HTTP server and ACP loop
    // have stopped. Dropping these owners removes the registry and socket.
    drop(http_state);
    drop(bridge);
    Ok(())
}

fn router(state: HttpState) -> Router {
    Router::new()
        .route("/", get(shell))
        .route("/events", get(events))
        .route("/interaction", post(interaction))
        .route("/actions/file-viewed", post(file_viewed))
        .route("/actions/file-unviewed", post(file_unviewed))
        .route("/actions/skim-acknowledge", post(skim_acknowledge))
        .route("/actions/skim-acknowledge-all", post(skim_acknowledge_all))
        .route("/actions/comment-add", post(comment_add))
        .route("/actions/comment-edit", post(comment_edit))
        .route("/actions/comment-reply", post(comment_reply))
        .route("/actions/comment-state", post(comment_state))
        .route("/actions/draft-accept", post(draft_accept))
        .route("/actions/draft-discard", post(draft_discard))
        .route("/actions/salience-set", post(salience_set))
        .route("/actions/salience-clear", post(salience_clear))
        .route("/actions/salience-promote", post(salience_promote))
        .route("/actions/salience-demote", post(salience_demote))
        .route("/actions/walkthrough-next", post(walkthrough_next))
        .route("/actions/walkthrough-prev", post(walkthrough_prev))
        .route("/actions/walkthrough-goto", post(walkthrough_goto))
        .route("/assets/app.css", get(stylesheet))
        .route("/assets/app.js", get(script))
        .route("/fragment/{region}", get(fragment))
        .route("/assets/extra.css", get(extra_stylesheet))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_guard,
        ))
        .with_state(state)
}

async fn security_guard(State(state): State<HttpState>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    let origin = match request.headers().get(header::ORIGIN) {
        Some(value) => match value.to_str() {
            Ok(value) => Some(value),
            Err(_) => return (StatusCode::FORBIDDEN, "invalid Origin header").into_response(),
        },
        None => None,
    };
    if let Err((status, message)) = validate_request(
        host,
        origin,
        request.uri().query(),
        &state.expected_host,
        &state.expected_origin,
        &state.token,
    ) {
        return (status, message).into_response();
    }

    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn validate_request(
    host: Option<&str>,
    origin: Option<&str>,
    query: Option<&str>,
    expected_host: &str,
    expected_origin: &str,
    token: &str,
) -> std::result::Result<(), (StatusCode, &'static str)> {
    if host != Some(expected_host) {
        return Err((StatusCode::BAD_REQUEST, "invalid Host header"));
    }
    if origin.is_some_and(|origin| origin != expected_origin) {
        return Err((StatusCode::FORBIDDEN, "cross-origin request rejected"));
    }
    if !request_token(query).is_some_and(|candidate| constant_time_eq(candidate, token)) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "missing or invalid capability token",
        ));
    }
    Ok(())
}

fn constant_time_eq(candidate: &str, expected: &str) -> bool {
    if candidate.len() != expected.len() {
        return false;
    }
    candidate
        .as_bytes()
        .iter()
        .zip(expected.as_bytes())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn request_token(query: Option<&str>) -> Option<&str> {
    let mut token = None;
    for pair in query?.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if key == "token" {
            if token.is_some() {
                return None;
            }
            token = Some(value);
        }
    }
    token
}

async fn shell(State(state): State<HttpState>) -> Html<String> {
    Html(render_shell(&state))
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        COMPONENT_CSS,
    )
}

async fn script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        COMPONENT_JS,
    )
}

async fn fragment(
    State(state): State<HttpState>,
    Path(region): Path<String>,
    request: Request,
) -> Response {
    let requested_generation = query_value(request.uri().query(), "generation")
        .and_then(|value| value.parse::<u64>().ok());
    let mode = match query_value(request.uri().query(), "mode") {
        Some("full") => RenderMode::Full,
        Some("guided") | None => RenderMode::Guided,
        Some(_) => return (StatusCode::BAD_REQUEST, "unknown fragment mode").into_response(),
    };
    let review = match state.review.read() {
        Ok(review) => review.clone(),
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "review projection unavailable",
            )
                .into_response();
        }
    };
    let region = match lookup_fragment(&review, &region, requested_generation) {
        Ok(region) => region.clone(),
        Err(error) => return error.into_response(),
    };
    Html(render_region(&region, true, mode)).into_response()
}

fn lookup_fragment<'a>(
    review: &'a WebReview,
    region: &str,
    generation: Option<u64>,
) -> std::result::Result<&'a ReadingRegion, (StatusCode, &'static str)> {
    if generation != Some(review.generation) {
        return Err((
            StatusCode::CONFLICT,
            "stale fragment generation; reload the review",
        ));
    }
    review
        .region(region)
        .ok_or((StatusCode::NOT_FOUND, "unknown review region"))
}

fn json_bad(rejection: axum::extract::rejection::JsonRejection) -> Response {
    (
        StatusCode::BAD_REQUEST,
        format!("invalid action JSON: {rejection}"),
    )
        .into_response()
}

async fn submit_action(
    state: HttpState,
    expected_generation: u64,
    command: ActionCommand,
) -> Response {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    if state
        .actions
        .send(ActionEnvelope {
            expected_generation,
            command,
            response: sender,
        })
        .await
        .is_err()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "review action service unavailable",
        )
            .into_response();
    }
    match receiver.await {
        Ok(Ok(result)) => Json(result).into_response(),
        Ok(Err(error)) => (error.status, error.message).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "review action service stopped",
        )
            .into_response(),
    }
}

async fn file_viewed(
    State(state): State<HttpState>,
    payload: std::result::Result<Json<FileViewedAction>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(payload)) = payload else {
        return json_bad(payload.unwrap_err());
    };
    submit_action(
        state,
        payload.expected_generation,
        ActionCommand::FileViewed {
            path: payload.path,
            viewed: true,
        },
    )
    .await
}

async fn file_unviewed(
    State(state): State<HttpState>,
    payload: std::result::Result<Json<FileViewedAction>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(payload)) = payload else {
        return json_bad(payload.unwrap_err());
    };
    submit_action(
        state,
        payload.expected_generation,
        ActionCommand::FileViewed {
            path: payload.path,
            viewed: false,
        },
    )
    .await
}

async fn skim_acknowledge(
    State(state): State<HttpState>,
    payload: std::result::Result<Json<AcknowledgeAction>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(payload)) = payload else {
        return json_bad(payload.unwrap_err());
    };
    let selection = match (payload.fold_id, payload.path) {
        (Some(id), None) => SkimSelection::StableId(id),
        (None, Some(path)) => SkimSelection::Target {
            path,
            line: payload.line,
            end_line: payload.end_line,
        },
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "provide exactly one of fold_id or path",
            )
                .into_response();
        }
    };
    submit_action(
        state,
        payload.expected_generation,
        ActionCommand::Acknowledge { selection },
    )
    .await
}

async fn skim_acknowledge_all(
    State(state): State<HttpState>,
    payload: std::result::Result<Json<GenerationAction>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(payload)) = payload else {
        return json_bad(payload.unwrap_err());
    };
    submit_action(
        state,
        payload.expected_generation,
        ActionCommand::Acknowledge {
            selection: SkimSelection::AllCurrent,
        },
    )
    .await
}

async fn comment_add(
    State(state): State<HttpState>,
    payload: std::result::Result<Json<CommentAddAction>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(payload)) = payload else {
        return json_bad(payload.unwrap_err());
    };
    submit_action(
        state.clone(),
        payload.expected_generation,
        ActionCommand::CommentAdd(payload),
    )
    .await
}
async fn comment_edit(
    State(state): State<HttpState>,
    payload: std::result::Result<Json<CommentEditAction>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(payload)) = payload else {
        return json_bad(payload.unwrap_err());
    };
    submit_action(
        state.clone(),
        payload.expected_generation,
        ActionCommand::CommentEdit(payload),
    )
    .await
}
async fn comment_reply(
    State(state): State<HttpState>,
    payload: std::result::Result<Json<CommentReplyAction>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(payload)) = payload else {
        return json_bad(payload.unwrap_err());
    };
    submit_action(
        state.clone(),
        payload.expected_generation,
        ActionCommand::CommentReply(payload),
    )
    .await
}
async fn comment_state(
    State(state): State<HttpState>,
    payload: std::result::Result<Json<CommentStateAction>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(payload)) = payload else {
        return json_bad(payload.unwrap_err());
    };
    submit_action(
        state,
        payload.expected_generation,
        ActionCommand::CommentState {
            id: payload.id,
            state: payload.state,
        },
    )
    .await
}
async fn draft_accept(
    State(state): State<HttpState>,
    payload: std::result::Result<Json<DraftAcceptAction>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(payload)) = payload else {
        return json_bad(payload.unwrap_err());
    };
    submit_action(
        state,
        payload.expected_generation,
        ActionCommand::DraftAccept {
            id: payload.id,
            body: payload.body,
            channel: payload.channel,
        },
    )
    .await
}
async fn draft_discard(
    State(state): State<HttpState>,
    payload: std::result::Result<Json<IdAction>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(payload)) = payload else {
        return json_bad(payload.unwrap_err());
    };
    submit_action(
        state,
        payload.expected_generation,
        ActionCommand::DraftDiscard { id: payload.id },
    )
    .await
}
async fn salience_action(
    state: HttpState,
    payload: SalienceAction,
    verb: SalienceVerb,
) -> Response {
    submit_action(
        state,
        payload.expected_generation,
        ActionCommand::Salience {
            verb,
            target: payload.target,
            salience: payload.salience,
            rationale: payload.rationale,
        },
    )
    .await
}
macro_rules! salience_handler {
    ($name:ident, $verb:expr) => {
        async fn $name(
            State(state): State<HttpState>,
            payload: std::result::Result<
                Json<SalienceAction>,
                axum::extract::rejection::JsonRejection,
            >,
        ) -> Response {
            let Ok(Json(payload)) = payload else {
                return json_bad(payload.unwrap_err());
            };
            salience_action(state, payload, $verb).await
        }
    };
}
salience_handler!(salience_set, SalienceVerb::Set);
salience_handler!(salience_clear, SalienceVerb::Clear);
salience_handler!(salience_promote, SalienceVerb::Promote);
salience_handler!(salience_demote, SalienceVerb::Demote);

async fn walkthrough_next(
    State(state): State<HttpState>,
    payload: std::result::Result<Json<GenerationAction>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(payload)) = payload else {
        return json_bad(payload.unwrap_err());
    };
    submit_action(
        state,
        payload.expected_generation,
        ActionCommand::Walkthrough {
            verb: WalkthroughVerb::Next,
            step_id: None,
            part: None,
        },
    )
    .await
}
async fn walkthrough_prev(
    State(state): State<HttpState>,
    payload: std::result::Result<Json<GenerationAction>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(payload)) = payload else {
        return json_bad(payload.unwrap_err());
    };
    submit_action(
        state,
        payload.expected_generation,
        ActionCommand::Walkthrough {
            verb: WalkthroughVerb::Prev,
            step_id: None,
            part: None,
        },
    )
    .await
}
async fn walkthrough_goto(
    State(state): State<HttpState>,
    payload: std::result::Result<
        Json<WalkthroughGotoAction>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Response {
    let Ok(Json(payload)) = payload else {
        return json_bad(payload.unwrap_err());
    };
    submit_action(
        state,
        payload.expected_generation,
        ActionCommand::Walkthrough {
            verb: WalkthroughVerb::Goto,
            step_id: Some(payload.step_id),
            part: payload.part,
        },
    )
    .await
}

async fn extra_stylesheet(State(state): State<HttpState>) -> Response {
    match state.extra_css.as_deref() {
        Some(css) => (
            [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
            css.to_string(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn interaction(
    State(state): State<HttpState>,
    Json(report): Json<InteractionReport>,
) -> Response {
    let result = state
        .interactions
        .lock()
        .map_err(|_| "interaction registry unavailable")
        .and_then(|mut interactions| interactions.report(report));
    if let Err(message) = result {
        return (StatusCode::BAD_REQUEST, message).into_response();
    }
    if let Ok(mut registration) = state.registration.lock() {
        let _ = registration.record_input(&state.base, &state.rev, &state.summary);
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn events(State(state): State<HttpState>, request: Request) -> Response {
    let query_generation = query_value(request.uri().query(), "generation")
        .and_then(|value| value.parse::<u64>().ok());
    let header_generation = request
        .headers()
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    // EventSource keeps the original query string while adding Last-Event-ID
    // on reconnect, so the header is authoritative once present.
    let client_generation = header_generation.or(query_generation);
    let tab_id = query_value(request.uri().query(), "tab")
        .filter(|tab_id| valid_tab_id(tab_id))
        .map(str::to_owned);
    let Some(tab_id) = tab_id else {
        return (StatusCode::BAD_REQUEST, "missing or invalid tab id").into_response();
    };
    let connection = state
        .interactions
        .lock()
        .ok()
        .and_then(|mut interactions| interactions.connect(&tab_id))
        .unwrap_or(0);
    let mut receiver = state.events.subscribe();
    let mut present = state.present_events.clone();
    let (sender, body_receiver) = tokio::sync::mpsc::channel(1);
    let state_for_stream = state.clone();
    let tab_for_stream = tab_id.clone();
    let mut shutdown = state.shutdown.clone();
    tokio::spawn(async move {
        let current = full_projection_event(&state_for_stream);
        if client_generation != Some(current.generation)
            && sender.send(sse_state_event(&current)).await.is_err()
        {
            return;
        }
        let initial_present = present.borrow_and_update().clone();
        if let Some(event) = initial_present
            && sender.send(sse_present_event(&event)).await.is_err()
        {
            if let Ok(mut interactions) = state_for_stream.interactions.lock() {
                interactions.disconnect(&tab_for_stream, connection);
            }
            return;
        }
        let mut disconnected_check = tokio::time::interval(Duration::from_secs(1));
        loop {
            let received = tokio::select! {
                _ = shutdown.changed() => None,
                _ = disconnected_check.tick() => {
                    if sender.is_closed() { None } else { continue }
                },
                changed = present.changed() => {
                    if changed.is_err() {
                        None
                    } else {
                        let event = present.borrow_and_update().clone();
                        if let Some(event) = event {
                            if sender.send(sse_present_event(&event)).await.is_err() {
                                None
                            } else {
                                continue;
                            }
                        } else {
                            continue;
                        }
                    }
                }
                received = receiver.recv() => Some(received),
            };
            let Some(received) = received else { break };
            match received {
                Ok(event) => {
                    if sender.send(sse_state_event(&event)).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let recovery = full_projection_event(&state_for_stream);
                    if sender.send(sse_state_event(&recovery)).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
        if let Ok(mut interactions) = state_for_stream.interactions.lock() {
            interactions.disconnect(&tab_for_stream, connection);
        }
    });
    Sse::new(ReceiverStream {
        receiver: body_receiver,
    })
    .keep_alive(KeepAlive::new().interval(SSE_KEEPALIVE).text("gander"))
    .into_response()
}

async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "not found")
}

fn render_shell(state: &HttpState) -> String {
    let review = state
        .review
        .read()
        .expect("web projection lock poisoned")
        .clone();
    let review = &review;
    let projection = &review.projection;
    let mut out = String::from(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Gander review</title><script>",
    );
    out.push_str(PREPAINT_SCRIPT);
    out.push_str("</script><style>");
    out.push_str(&state.theme_css);
    out.push_str("</style><link rel=\"stylesheet\" href=\"/assets/app.css?token=");
    escape_to(&mut out, &state.token);
    out.push_str("\">");
    if state.extra_css.is_some() {
        out.push_str("<link rel=\"stylesheet\" href=\"/assets/extra.css?token=");
        escape_to(&mut out, &state.token);
        out.push_str("\">");
    }
    out.push_str("</head><body data-mode=\"guided\" data-token=\"");
    escape_to(&mut out, &state.token);
    out.push_str("\" data-generation=\"");
    out.push_str(&review.generation.to_string());
    out.push_str(
        "\"><header class=\"topbar\"><div><p class=\"eyebrow\">Gander local review</p><strong>",
    );
    escape_to(&mut out, &state.target);
    out.push_str("</strong></div><div class=\"controls\"><label class=\"search\">Search <input id=\"review-search\" type=\"search\" placeholder=\"File, code, or comment\"></label><button class=\"theme-toggle\" type=\"button\" data-theme-toggle aria-label=\"Cycle color scheme\">Theme: <span data-theme-label>system</span></button><button id=\"mode-switch\" type=\"button\" aria-pressed=\"false\">Full review</button></div></header><aside id=\"presenter\" class=\"presenter\" hidden aria-live=\"polite\"><span id=\"presenter-status\">Following presenter</span><button id=\"presenter-rejoin\" type=\"button\" hidden>Following paused — rejoin</button><span id=\"presenter-edge\" class=\"presenter-edge\" hidden></span><p id=\"presenter-note\" class=\"presenter-note\" hidden></p></aside><div class=\"app-layout\">");
    out.push_str(&render_file_tree_html(review));
    out.push_str("<main>");
    out.push_str(&render_overview_with_target(review, &state.target));
    out.push_str("<section id=\"review-stream\" class=\"review-stream\" aria-label=\"Shared review stream\"><div class=\"stream-heading\"><div><p class=\"eyebrow\">Shared projection</p><h2>Review stream</h2></div><p class=\"guided-only\">Skims stay compact; spotlights carry narration.</p><p class=\"full-only\">Every file and line is visible. Salience remains in the margin.</p></div><form id=\"comment-composer\" class=\"comment-composer\"><label>Comment on selected row (or general)<textarea name=\"body\" required placeholder=\"Leave durable review feedback…\"></textarea></label><label>Channel <select name=\"channel\"><option value=\"\">Infer safely</option><option value=\"delegation\">Delegation</option><option value=\"collaboration\">Collaboration</option><option value=\"note\">Private note</option><option value=\"onboarding\">Onboarding</option></select></label><button type=\"submit\">Save comment</button><span class=\"action-status\" role=\"status\"></span></form>");
    for (index, region) in projection.regions.iter().enumerate() {
        if index < INITIAL_REGION_WINDOW || matches!(region.kind, ReadingRegionKind::Chapter(_)) {
            out.push_str(&render_region(region, false, RenderMode::Guided));
        } else {
            out.push_str("<section id=\"");
            escape_to(&mut out, &region.id);
            out.push_str("\" class=\"region region-skeleton\" data-region=\"");
            escape_to(&mut out, &region.id);
            out.push_str("\"><div class=\"skeleton-label\"><strong>");
            escape_to(&mut out, &web_render::region_label(region));
            out.push_str("</strong><span>");
            escape_to(&mut out, &region.member_paths.join(", "));
            out.push_str("</span></div><div class=\"skeleton-lines\" aria-hidden=\"true\"></div><noscript><p>JavaScript is required only to load this offscreen region.</p></noscript></section>");
        }
    }
    out.push_str("</section>");
    out.push_str(&render_footer(review));
    out.push_str("</main></div><script src=\"/assets/app.js?token=");
    escape_to(&mut out, &state.token);
    out.push_str("\" defer></script><script>");
    out.push_str(THEME_CONTROL_SCRIPT);
    out.push_str("</script></body></html>");
    out
}

fn render_overview(review: &WebReview) -> String {
    web_render::render_overview(
        review,
        &review.target,
        RenderOptions::live(RenderMode::Guided, false),
    )
}

fn render_overview_with_target(review: &WebReview, target: &str) -> String {
    web_render::render_overview(
        review,
        target,
        RenderOptions::live(RenderMode::Guided, false),
    )
}

fn render_coverage(review: &WebReview) -> String {
    web_render::render_coverage(review)
}

fn render_footer(review: &WebReview) -> String {
    web_render::render_footer(review)
}

fn render_file_tree_html(review: &WebReview) -> String {
    web_render::render_file_tree(review, RenderOptions::live(RenderMode::Guided, false))
}

fn render_region(region: &ReadingRegion, fragment: bool, mode: RenderMode) -> String {
    web_render::render_region(region, mode, RenderOptions::live(mode, fragment))
}

fn query_value<'a>(query: Option<&'a str>, key: &str) -> Option<&'a str> {
    query?.split('&').find_map(|pair| {
        let (candidate, value) = pair.split_once('=')?;
        (candidate == key).then_some(value)
    })
}

fn escape_to(out: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(character),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_acp_requests(
    bridge: &mut AcpBridge,
    session: &mut ReviewSession,
    state_path: &std::path::Path,
    baseline: &mut ReviewState,
    watcher: &mut WebWatcher,
    presentation: &mut Option<WebPresentationState>,
    present_sequence: &mut u64,
    present_tx: &tokio::sync::watch::Sender<Option<Arc<PresentEvent>>>,
    state: &HttpState,
) {
    apply_controlling_focus(session, &state.interactions);
    let (_, _, commands, mutations) = bridge.drain_ui_commands(session);
    for request in mutations {
        let result = crate::tui::persist_acp_review_mutation(
            request.mutation.clone(),
            session,
            state_path,
            Some(baseline),
            &ReviewStateTombstones::default(),
        )
        .map_err(|error| (-32000, error.to_string()));
        if result.is_ok() {
            *baseline = session.to_state();
        }
        request.respond(result);
    }
    for request in commands {
        let command = request.command.clone();
        let result = apply_web_present_command(
            command.clone(),
            session,
            watcher,
            baseline,
            presentation,
            &state.interactions,
        );
        if let Err((-32001, message)) = &result {
            publish_present_event(
                session,
                present_sequence,
                present_tx,
                "pending",
                presentation,
                None,
                Some(format!("Presenter waiting: {message}")),
            );
        } else if result.is_ok() && !matches!(command, PresentCommand::Status) {
            let (label, explicit_target, note) = match command {
                PresentCommand::Start => ("start", None, None),
                PresentCommand::End => ("end", None, None),
                PresentCommand::Next => ("next", None, None),
                PresentCommand::Prev => ("prev", None, None),
                PresentCommand::GotoIndex(_) | PresentCommand::GotoStep(_) => ("goto", None, None),
                PresentCommand::Focus {
                    path,
                    line,
                    end_line,
                    note,
                } => (
                    "focus",
                    Some(resolve_present_target(
                        session,
                        &path,
                        Some(line),
                        Some(end_line.unwrap_or(line)),
                    )),
                    note,
                ),
                PresentCommand::Reload => ("reload", None, None),
                PresentCommand::Status => unreachable!(),
            };
            publish_present_event(
                session,
                present_sequence,
                present_tx,
                label,
                presentation,
                explicit_target.flatten(),
                note,
            );
        }
        request.respond(result);
    }
}

fn apply_web_present_command(
    command: PresentCommand,
    session: &mut ReviewSession,
    watcher: &mut WebWatcher,
    baseline: &mut ReviewState,
    presentation: &mut Option<WebPresentationState>,
    interactions: &Arc<Mutex<WebInteractions>>,
) -> std::result::Result<Value, (i64, String)> {
    if let Some(mode) = interactions
        .lock()
        .ok()
        .and_then(|tabs| tabs.controlling().and_then(|(_, tab)| tab.busy.clone()))
    {
        return Err((-32001, format!("user is busy: {mode}")));
    }
    match command {
        PresentCommand::Status => Ok(web_present_status(session, presentation)),
        PresentCommand::Start => {
            if presentation.is_none() {
                if session.spotlight_count() == 0 {
                    return Err((
                        -32002,
                        "nothing to present — no current Spotlight regions; author a durable walkthrough and attention map".to_owned(),
                    ));
                }
                let (step_id, part) = session
                    .jump_to_spotlight_index(0)
                    .ok_or_else(|| (-32002, "first Spotlight is unavailable".to_owned()))?;
                *presentation = Some(WebPresentationState {
                    identity: SpotlightIdentity { step_id, part },
                    index: 0,
                    stale: false,
                });
            }
            Ok(web_present_status(session, presentation))
        }
        PresentCommand::End => {
            *presentation = None;
            Ok(web_present_status(session, presentation))
        }
        PresentCommand::Next => {
            let current = presentation
                .as_ref()
                .ok_or_else(|| (-32002, "presentation is not active".to_owned()))?;
            if current.stale {
                return Err((
                    -32002,
                    "current Spotlight is stale; use present/goto to choose a current target"
                        .to_owned(),
                ));
            }
            let target = (current.index + 1).min(session.spotlight_count().saturating_sub(1));
            goto_web_spotlight(session, presentation, target)?;
            Ok(web_present_status(session, presentation))
        }
        PresentCommand::Prev => {
            let current = presentation
                .as_ref()
                .ok_or_else(|| (-32002, "presentation is not active".to_owned()))?;
            if current.stale {
                return Err((
                    -32002,
                    "current Spotlight is stale; use present/goto to choose a current target"
                        .to_owned(),
                ));
            }
            let target = current.index.saturating_sub(1);
            goto_web_spotlight(session, presentation, target)?;
            Ok(web_present_status(session, presentation))
        }
        PresentCommand::GotoIndex(index) => {
            if presentation.is_none() {
                return Err((-32002, "presentation is not active".to_owned()));
            }
            if index >= session.spotlight_count() {
                return Err((-32602, format!("slide index {index} out of range")));
            }
            goto_web_spotlight(session, presentation, index)?;
            Ok(web_present_status(session, presentation))
        }
        PresentCommand::GotoStep(step_id) => {
            if presentation.is_none() {
                return Err((-32002, "presentation is not active".to_owned()));
            }
            let index = session
                .spotlight_index_for_step(&step_id)
                .ok_or_else(|| (-32602, format!("unknown step_id: {step_id}")))?;
            goto_web_spotlight(session, presentation, index)?;
            Ok(web_present_status(session, presentation))
        }
        PresentCommand::Focus {
            path,
            line,
            end_line,
            ..
        } => {
            let row = validate_focus_target(session, &path, line, end_line)?;
            session.select_stream_row(row, true);
            session.focus = Focus::Diff;
            Ok(json!({ "ok": true, "path": path, "line": line, "end_line": end_line }))
        }
        PresentCommand::Reload => {
            watcher
                .reload_local_state(session, baseline)
                .map_err(|error| (-32000, error.to_string()))?;
            reconcile_web_presentation(session, presentation);
            Ok(web_present_status(session, presentation))
        }
    }
}

fn validate_focus_target(
    session: &ReviewSession,
    path: &str,
    line: usize,
    end_line: Option<usize>,
) -> std::result::Result<usize, (i64, String)> {
    if !session.files.iter().any(|file| file.path == path) {
        return Err((-32602, format!("path is not in the diff: {path}")));
    }
    let requested_end = end_line.unwrap_or(line);
    if requested_end < line {
        return Err((
            -32602,
            "end_line must be greater than or equal to line".to_owned(),
        ));
    }
    session
        .review_stream()
        .rows
        .iter()
        .enumerate()
        .find_map(|(index, row)| {
            (row.path.as_deref() == Some(path)
                && row
                    .anchor
                    .as_ref()
                    .and_then(crate::anchor::CommentAnchor::line)
                    .is_some_and(|anchor| line <= anchor && anchor <= requested_end))
            .then_some(index)
        })
        .ok_or_else(|| {
            (
                -32602,
                format!("location is not in the diff: {path}:{line}"),
            )
        })
}

fn goto_web_spotlight(
    session: &mut ReviewSession,
    presentation: &mut Option<WebPresentationState>,
    index: usize,
) -> std::result::Result<(), (i64, String)> {
    let (step_id, part) = session
        .jump_to_spotlight_index(index)
        .ok_or_else(|| (-32002, format!("Spotlight {index} is unavailable")))?;
    *presentation = Some(WebPresentationState {
        identity: SpotlightIdentity { step_id, part },
        index,
        stale: false,
    });
    Ok(())
}

fn reconcile_web_presentation(
    session: &ReviewSession,
    presentation: &mut Option<WebPresentationState>,
) -> bool {
    let Some(current) = presentation.as_mut() else {
        return false;
    };
    match session.spotlight_index_for_identity(&current.identity.step_id, current.identity.part) {
        Some(index) => {
            let changed = current.index != index || current.stale;
            current.index = index;
            current.stale = false;
            changed
        }
        None => {
            let changed = !current.stale;
            current.stale = true;
            changed
        }
    }
}

fn web_present_status(
    session: &ReviewSession,
    presentation: &Option<WebPresentationState>,
) -> Value {
    let Some(presentation) = presentation else {
        return json!({ "active": false });
    };
    let stream = session.review_stream();
    let current = (!presentation.stale)
        .then(|| stream.spotlights.get(presentation.index))
        .flatten()
        .filter(|spotlight| {
            spotlight.step_id == presentation.identity.step_id
                && spotlight.part == presentation.identity.part
        });
    json!({
        "active": true,
        "slide_index": presentation.index,
        "slide_count": stream.spotlights.len(),
        "view": "focus",
        "current": current.map(|spotlight| json!({
            "step_id": spotlight.step_id,
            "part": spotlight.part,
            "path": spotlight.target.file,
            "line": spotlight.target.line,
            "end_line": spotlight.target.end_line,
            "stale": false,
        })).unwrap_or_else(|| json!({
            "step_id": presentation.identity.step_id,
            "part": presentation.identity.part,
            "path": null,
            "line": null,
            "end_line": null,
            "stale": true,
        })),
    })
}

fn publish_present_event(
    session: &ReviewSession,
    sequence: &mut u64,
    sender: &tokio::sync::watch::Sender<Option<Arc<PresentEvent>>>,
    command: &'static str,
    presentation: &Option<WebPresentationState>,
    explicit_target: Option<PresentTarget>,
    note: Option<String>,
) {
    *sequence = sequence.saturating_add(1);
    let target = explicit_target.or_else(|| presentation_target(session, presentation));
    sender.send_replace(Some(Arc::new(PresentEvent {
        sequence: *sequence,
        command,
        status: web_present_status(session, presentation),
        target,
        note,
    })));
}

fn presentation_target(
    session: &ReviewSession,
    presentation: &Option<WebPresentationState>,
) -> Option<PresentTarget> {
    let current = presentation.as_ref().filter(|current| !current.stale)?;
    let stream = session.review_stream();
    let spotlight = stream.spotlights.get(current.index)?.clone();
    if spotlight.step_id != current.identity.step_id || spotlight.part != current.identity.part {
        return None;
    }
    resolve_present_target(
        session,
        spotlight.target.file.as_deref()?,
        spotlight.target.line,
        spotlight.target.end_line.or(spotlight.target.line),
    )
}

fn resolve_present_target(
    session: &ReviewSession,
    path: &str,
    line: Option<usize>,
    end_line: Option<usize>,
) -> Option<PresentTarget> {
    let projection = session.reading_projection();
    let mut chosen = None;
    for region in &projection.regions {
        for row in &region.rows {
            if row.path.as_deref() != Some(path) {
                continue;
            }
            let anchor_line = row
                .anchor
                .as_ref()
                .and_then(crate::anchor::CommentAnchor::line);
            let matches = match (line, end_line, anchor_line) {
                (Some(start), Some(end), Some(candidate)) => start <= candidate && candidate <= end,
                (Some(start), None, Some(candidate)) => start == candidate,
                (None, _, _) => true,
                _ => false,
            };
            if matches {
                chosen = Some((region.id.clone(), web_render::dom_row_id(&row.id)));
                break;
            }
        }
        if chosen.is_some() {
            break;
        }
    }
    Some(PresentTarget {
        path: path.to_owned(),
        line,
        end_line,
        region_id: chosen.as_ref().map(|(region, _)| region.clone()),
        row_id: chosen.map(|(_, row)| row),
    })
}

fn apply_controlling_focus(
    session: &mut ReviewSession,
    interactions: &Arc<Mutex<WebInteractions>>,
) {
    let focus = interactions
        .lock()
        .ok()
        .and_then(|tabs| tabs.controlling().and_then(|(_, tab)| tab.focus.clone()));
    let Some(focus) = focus else { return };
    let stream = session.review_stream();
    let row = stream.rows.iter().enumerate().find_map(|(index, row)| {
        if row.path.as_deref() != focus.path.as_deref() {
            return None;
        }
        let anchor = row.anchor.as_ref()?;
        let matches = match anchor {
            crate::anchor::CommentAnchor::Line {
                old_line,
                new_line,
                hunk_header,
                ..
            } => {
                focus.old_line.is_some_and(|line| Some(line) == *old_line)
                    || focus.new_line.is_some_and(|line| Some(line) == *new_line)
                    || (focus.old_line.is_none()
                        && focus.new_line.is_none()
                        && focus.hunk_header.as_deref() == Some(hunk_header.as_str()))
            }
            _ => focus.old_line.is_none() && focus.new_line.is_none(),
        };
        matches.then_some(index)
    });
    drop(stream);
    if let Some(row) = row {
        session.select_stream_row(row, true);
    }
    session.focus = if focus.pane == "files" {
        Focus::Files
    } else {
        Focus::Diff
    };
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn render_theme_css(config: &ThemeConfig) -> String {
    web_render::render_theme_css(config)
}

fn read_extra_css(path: &std::path::Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| {
        format!(
            "failed to read [web] extra-css stylesheet at {}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{
            ChapterHeader, Coverage, DiffRow, DiffRowKind, ReadingAnnotation,
            ReadingAnnotationSource, ReadingProjection, ReadingRow, SkimFold,
        },
        diff::DiffLineKind,
        jj::{JjChangeSummary, JjOperationSummary, ReviewTarget, TargetAuthor},
        state::{AttentionProgressTarget, Comment, Walkthrough, WalkthroughStep},
        theme::Rgb,
        web_render::GuideFile,
    };
    use std::{
        path::Path as FsPath,
        sync::atomic::{AtomicUsize, Ordering},
    };

    fn row(id: &str, path: &str, text: &str, salience: Salience) -> ReadingRow {
        ReadingRow {
            id: id.into(),
            path: Some(path.into()),
            anchor: None,
            salience: Some(salience),
            diff: Some(DiffRow {
                old_lineno: None,
                new_lineno: Some(1),
                prefix: "+",
                text: text.into(),
                syntax: Vec::new(),
                emphasis: Vec::new(),
                kind: DiffRowKind::DiffLine(DiffLineKind::Added),
                hunk_index: Some(0),
                anchor: None,
                gap: None,
                semantic_key: None,
                semantic_parent_key: None,
                semantic_occurrence: 0,
                semantic_total: 1,
                logical_range: None,
                old_logical_range: None,
            }),
            annotations: Vec::new(),
        }
    }

    fn review_fixture() -> WebReview {
        let chapter = ChapterHeader {
            change_id: "change-1".into(),
            description: "Core behavior".into(),
            bookmarks: "feature".into(),
            additions: 7,
            deletions: 1,
        };
        let mut first = row(
            "row-0",
            "src/<core>.rs",
            "<script>bad()</script>",
            Salience::Spotlight,
        );
        first.annotations.push(ReadingAnnotation {
            owner_row_id: "row-0".into(),
            source: ReadingAnnotationSource::Comment(Comment {
                id: "comment".into(),
                path: Some("src/<core>.rs".into()),
                body: "Thread <unsafe>".into(),
                ..Default::default()
            }),
        });
        let mut regions = vec![
            ReadingRegion {
                id: "chapter-one".into(),
                kind: ReadingRegionKind::Chapter(chapter.clone()),
                member_paths: vec!["src/<core>.rs".into()],
                rows: Vec::new(),
            },
            ReadingRegion {
                id: "file-core".into(),
                kind: ReadingRegionKind::File {
                    file_index: 0,
                    path: "src/<core>.rs".into(),
                },
                member_paths: vec!["src/<core>.rs".into()],
                rows: vec![first],
            },
            ReadingRegion {
                id: "fold-generated".into(),
                kind: ReadingRegionKind::Skim(SkimFold {
                    id: "shared-fold".into(),
                    target: AttentionProgressTarget::default(),
                    rationale: "generated churn".into(),
                    files: vec!["generated.lock".into()],
                    file_indexes: vec![1],
                    additions: 20,
                    deletions: 3,
                    expanded: false,
                    acknowledged: false,
                    whole_files: std::collections::BTreeSet::from(["generated.lock".into()]),
                    hidden_rows: Vec::new(),
                }),
                member_paths: vec!["generated.lock".into()],
                rows: vec![
                    ReadingRow {
                        id: "shared-fold".into(),
                        path: Some("generated.lock".into()),
                        anchor: None,
                        salience: Some(Salience::Skim),
                        diff: None,
                        annotations: Vec::new(),
                    },
                    row("row-lock", "generated.lock", "version = 99", Salience::Skim),
                ],
            },
        ];
        for index in 2..7 {
            let path = format!("src/file-{index}.rs");
            regions.push(ReadingRegion {
                id: format!("file-{index}"),
                kind: ReadingRegionKind::File {
                    file_index: index,
                    path: path.clone(),
                },
                member_paths: vec![path.clone()],
                rows: vec![row(
                    &format!("row-{index}"),
                    &path,
                    "let value = true;",
                    Salience::Supporting,
                )],
            });
        }
        let mut file_region_ids = std::collections::BTreeMap::new();
        file_region_ids.insert("src/<core>.rs".into(), "file-core".into());
        file_region_ids.insert("generated.lock".into(), "fold-generated".into());
        for index in 2..7 {
            file_region_ids.insert(format!("src/file-{index}.rs"), format!("file-{index}"));
        }
        let files = file_region_ids
            .iter()
            .enumerate()
            .map(|(index, (path, region))| GuideFile {
                path: path.clone(),
                additions: index + 1,
                deletions: 0,
                viewed: index == 0,
                generated: path == "generated.lock",
                region_id: Some(region.clone()),
            })
            .collect();
        WebReview {
            projection: ReadingProjection {
                summary: "7 files, attention ready".into(),
                coverage: Coverage {
                    covered: 1,
                    total: 3,
                },
                spotlight_count: 2,
                skim_count: 1,
                skim_files: 1,
                chapters: vec![chapter],
                has_walkthrough: true,
                regions,
                file_region_ids,
            },
            files,
            generation: 42,
            target: "main..@".into(),
        }
    }

    fn http_state(review: WebReview) -> HttpState {
        let registry_dir =
            std::env::temp_dir().join(format!("gander-web-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&registry_dir).unwrap();
        let registration = InstanceRegistration::register(
            &registry_dir,
            InstanceInfo {
                pid: std::process::id(),
                workspace_root: "/repo".into(),
                base: "main".into(),
                rev: "@".into(),
                summary: "summary".into(),
                socket_path: registry_dir.join("socket"),
                started_at: Utc::now(),
                last_input_at: Utc::now(),
            },
        )
        .unwrap();
        let (actions, _receiver) = tokio::sync::mpsc::channel(1);
        HttpState {
            token: Arc::from("safe-token"),
            expected_host: Arc::from("127.0.0.1:8123"),
            expected_origin: Arc::from("http://127.0.0.1:8123"),
            summary: Arc::from("summary"),
            base: Arc::from("main"),
            rev: Arc::from("@"),
            target: Arc::from("main..@"),
            theme_css: Arc::from(render_theme_css(&ThemeConfig::default())),
            review: Arc::new(RwLock::new(review)),
            events: tokio::sync::broadcast::channel(SSE_BROADCAST_CAPACITY).0,
            present_events: tokio::sync::watch::channel(None).1,
            interactions: Arc::new(Mutex::new(WebInteractions::default())),
            shutdown: tokio::sync::watch::channel(false).1,
            extra_css: None,
            registration: Arc::new(Mutex::new(registration)),
            actions,
        }
    }

    #[test]
    fn projection_diff_is_monotonic_surgical_and_carries_removals() {
        let previous = review_fixture();
        let mut next = review_fixture();
        next.generation = previous.generation + 1;
        next.projection.coverage.covered += 1;
        next.projection.regions.remove(1);
        let event = diff_projection(&previous, &next);
        assert_eq!(event.generation, 43);
        assert!(!event.full);
        assert!(
            event
                .patches
                .iter()
                .any(|patch| patch.id == "coverage" && !patch.remove)
        );
        assert!(
            event
                .patches
                .iter()
                .any(|patch| patch.id == "overview" && !patch.remove)
        );
        assert!(
            event
                .patches
                .iter()
                .any(|patch| patch.id == "footer" && !patch.remove)
        );
        assert!(
            event
                .patches
                .iter()
                .any(|patch| patch.id == "file-core" && patch.remove)
        );
        assert!(
            !event
                .patches
                .iter()
                .any(|patch| patch.id == "fold-generated")
        );
    }

    #[test]
    fn full_projection_recovers_absent_or_dropped_generation() {
        let state = http_state(review_fixture());
        let event = full_projection_event(&state);
        assert!(event.full);
        assert_eq!(event.generation, 42);
        assert!(event.patches.iter().any(|patch| patch.id == "overview"));
        assert!(event.patches.iter().any(|patch| patch.id == "footer"));
        assert!(event.patches.iter().any(|patch| patch.id == "file-core"));
        assert_eq!(event.order.first().map(String::as_str), Some("chapter-one"));
    }

    #[derive(Default)]
    struct JjCounts {
        snapshots: AtomicUsize,
        fingerprints: AtomicUsize,
        diffs: AtomicUsize,
        operations: AtomicUsize,
    }

    struct WatchJj {
        counts: Arc<JjCounts>,
        fingerprint: Arc<Mutex<String>>,
        diff: String,
    }

    impl JjBackend for WatchJj {
        fn diff(&self, _repo: &FsPath, _target: &ReviewTarget) -> Result<String> {
            self.counts.diffs.fetch_add(1, Ordering::SeqCst);
            Ok(self.diff.clone())
        }
        fn change_summaries(&self, _repo: &FsPath) -> Result<Vec<JjChangeSummary>> {
            Ok(Vec::new())
        }
        fn stack_changes(
            &self,
            _repo: &FsPath,
            _target: &ReviewTarget,
        ) -> Result<Vec<JjChangeSummary>> {
            Ok(Vec::new())
        }
        fn target_author(&self, _repo: &FsPath, _target: &ReviewTarget) -> Result<TargetAuthor> {
            Ok(TargetAuthor::default())
        }
        fn snapshot_working_copy(&self, _repo: &FsPath) -> Result<()> {
            self.counts.snapshots.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn change_fingerprint(&self, _repo: &FsPath, _target: &ReviewTarget) -> Result<String> {
            self.counts.fingerprints.fetch_add(1, Ordering::SeqCst);
            Ok(self.fingerprint.lock().unwrap().clone())
        }
        fn operations(&self, _repo: &FsPath) -> Result<Vec<JjOperationSummary>> {
            self.counts.operations.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
        fn diff_at_operation(
            &self,
            _repo: &FsPath,
            _target: &ReviewTarget,
            _operation_id: &str,
        ) -> Result<String> {
            Ok(String::new())
        }
        fn file_contents(&self, _repo: &FsPath, _rev: &str, _path: &str) -> Result<String> {
            Ok(String::new())
        }
        fn run_command(&self, _repo: &FsPath, _args: &[String]) -> Result<String> {
            panic!("watcher must not mutate jj")
        }
    }

    #[test]
    fn repo_watcher_has_one_snapshot_point_and_read_only_refresh_reads() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(raw).unwrap(),
            ReviewState::default(),
        );
        let counts = Arc::new(JjCounts::default());
        let fingerprint = Arc::new(Mutex::new("one".to_owned()));
        let dir = tempfile::tempdir().unwrap();
        let mut watcher = WebWatcher {
            state_path: dir.path().join("state.json"),
            overlay_path: dir.path().join("agent.json"),
            state_mtime: None,
            overlay_mtime: None,
            repo_fingerprint: None,
            last_repo_poll: None,
            jj: Box::new(WatchJj {
                counts: counts.clone(),
                fingerprint: fingerprint.clone(),
                diff: raw.into(),
            }),
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
        };
        watcher.poll_repo(&mut session);
        *fingerprint.lock().unwrap() = "two".into();
        watcher.last_repo_poll = None;
        watcher.poll_repo(&mut session);
        assert_eq!(counts.snapshots.load(Ordering::SeqCst), 2);
        assert_eq!(counts.fingerprints.load(Ordering::SeqCst), 2);
        assert_eq!(counts.diffs.load(Ordering::SeqCst), 1);
        assert_eq!(counts.operations.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn durable_state_and_overlay_invalidate_the_shared_projection() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(raw).unwrap(),
            ReviewState::default(),
        );
        let mut baseline = session.to_state();
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let overlay_path = dir.path().join("agent.json");
        let mut external = baseline.clone();
        external.comments.push(Comment {
            id: "external".into(),
            path: Some("a.rs".into()),
            line: Some(1),
            body: "from the TUI".into(),
            ..Comment::default()
        });
        external.save(&state_path).unwrap();
        let counts = Arc::new(JjCounts::default());
        let mut watcher = WebWatcher {
            state_path,
            overlay_path,
            state_mtime: None,
            overlay_mtime: None,
            repo_fingerprint: None,
            last_repo_poll: None,
            jj: Box::new(WatchJj {
                counts,
                fingerprint: Arc::new(Mutex::new(String::new())),
                diff: raw.into(),
            }),
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
        };
        let before = session.stream_inputs_generation();
        watcher.poll_files(&mut session, &mut baseline);
        assert!(session.stream_inputs_generation() > before);
        assert!(
            session
                .comments
                .iter()
                .any(|comment| comment.id == "external")
        );
        let after_state = session.stream_inputs_generation();
        crate::agent::AgentOverlay {
            version: crate::agent::AGENT_OVERLAY_VERSION,
            ordering: vec!["b.rs".into(), "a.rs".into()],
            ..Default::default()
        }
        .save(&watcher.overlay_path)
        .unwrap();
        watcher.poll_files(&mut session, &mut baseline);
        assert!(session.stream_inputs_generation() > after_state);
    }

    #[test]
    fn sse_backpressure_is_bounded_and_keepalive_is_configured() {
        assert_eq!(SSE_BROADCAST_CAPACITY, 16);
        assert_eq!(SSE_KEEPALIVE, Duration::from_secs(15));
        let (sender, mut receiver) = tokio::sync::broadcast::channel::<usize>(1);
        sender.send(1).unwrap();
        sender.send(2).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            assert!(matches!(
                receiver.recv().await,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(1))
            ));
            assert_eq!(receiver.recv().await.unwrap(), 2);
        });
    }

    #[test]
    fn token_query_is_exact_and_unique() {
        assert_eq!(request_token(Some("token=abc")), Some("abc"));
        assert_eq!(request_token(Some("x=1&token=abc")), Some("abc"));
        assert_eq!(request_token(None), None);
        assert_eq!(request_token(Some("token=a&token=b")), None);
        assert_eq!(request_token(Some("tokenish=abc")), None);
    }

    #[test]
    fn centralized_guard_rejects_missing_and_wrong_tokens() {
        let check = |query| {
            validate_request(
                Some("127.0.0.1:8123"),
                None,
                query,
                "127.0.0.1:8123",
                "http://127.0.0.1:8123",
                "secret",
            )
        };
        assert_eq!(check(None).unwrap_err().0, StatusCode::UNAUTHORIZED);
        assert_eq!(
            check(Some("token=wrong")).unwrap_err().0,
            StatusCode::UNAUTHORIZED
        );
        assert!(check(Some("token=secret")).is_ok());
    }

    #[test]
    fn centralized_guard_rejects_dns_rebinding_and_cross_origin_probes() {
        let check = |host, origin| {
            validate_request(
                host,
                origin,
                Some("token=secret"),
                "127.0.0.1:8123",
                "http://127.0.0.1:8123",
                "secret",
            )
        };
        assert_eq!(check(None, None).unwrap_err().0, StatusCode::BAD_REQUEST);
        assert_eq!(
            check(Some("attacker.test:8123"), None).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            check(Some("127.0.0.1:8123"), Some("https://attacker.test"))
                .unwrap_err()
                .0,
            StatusCode::FORBIDDEN
        );
        assert!(check(Some("127.0.0.1:8123"), Some("http://127.0.0.1:8123")).is_ok());
    }

    #[test]
    fn action_payloads_are_strict_and_require_generation() {
        assert!(
            serde_json::from_str::<FileViewedAction>(r#"{"expected_generation":4,"path":"a.rs"}"#)
                .is_ok()
        );
        assert!(serde_json::from_str::<FileViewedAction>(r#"{"path":"a.rs"}"#).is_err());
        assert!(
            serde_json::from_str::<FileViewedAction>(
                r#"{"expected_generation":4,"path":"a.rs","surprise":true}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<SalienceAction>(
                r#"{"expected_generation":4,"target":{"path":"a.rs","extra":1}}"#
            )
            .is_err()
        );
        let edit: CommentEditAction = serde_json::from_str(
            r#"{"expected_generation":4,"id":"comment","kind":null,"action":"fix"}"#,
        )
        .unwrap();
        assert_eq!(edit.kind, Some(None));
        assert_eq!(edit.action, Some(Some(ActionIntent::Fix)));
    }

    fn action_fixture() -> (
        ReviewSession,
        ReviewState,
        tempfile::TempDir,
        WebWatcher,
        HttpState,
    ) {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(raw).unwrap(),
            ReviewState::default(),
        );
        let baseline = session.to_state();
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        baseline.save(&state_path).unwrap();
        let watcher = WebWatcher {
            state_path,
            overlay_path: dir.path().join("agent.json"),
            state_mtime: None,
            overlay_mtime: None,
            repo_fingerprint: None,
            last_repo_poll: None,
            jj: Box::new(WatchJj {
                counts: Arc::new(JjCounts::default()),
                fingerprint: Arc::new(Mutex::new(String::new())),
                diff: raw.into(),
            }),
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
        };
        let http = http_state(WebReview::from_session(&session));
        (session, baseline, dir, watcher, http)
    }

    #[test]
    fn action_generation_is_preconditioned_and_merge_save_preserves_concurrent_state() {
        let (mut session, mut baseline, _dir, mut watcher, http) = action_fixture();
        let generation = http.review.read().unwrap().generation;
        let stale = process_action(
            generation.saturating_add(1),
            ActionCommand::FileViewed {
                path: "a.rs".into(),
                viewed: true,
            },
            &mut session,
            &watcher.state_path.clone(),
            &mut baseline,
            &mut watcher,
            &http,
        )
        .unwrap_err();
        assert_eq!(stale.status, StatusCode::CONFLICT);

        let mut external = ReviewState::load_or_default(&watcher.state_path).unwrap();
        external.comments.push(Comment {
            id: "from-tui".into(),
            body: "concurrent".into(),
            ..Comment::default()
        });
        external.save(&watcher.state_path).unwrap();
        let state_path = watcher.state_path.clone();
        let result = process_action(
            generation,
            ActionCommand::FileViewed {
                path: "a.rs".into(),
                viewed: true,
            },
            &mut session,
            &state_path,
            &mut baseline,
            &mut watcher,
            &http,
        )
        .unwrap();
        assert!(result.generation > generation);
        let saved = ReviewState::load_or_default(&state_path).unwrap();
        assert!(saved.files["a.rs"].viewed);
        assert!(
            saved
                .comments
                .iter()
                .any(|comment| comment.id == "from-tui")
        );
    }

    #[test]
    fn onboarding_request_infers_delegation_and_preserves_exact_source_location() {
        let (mut session, _baseline, _dir, mut watcher, _http) = action_fixture();
        let mut seeded = session.to_state();
        let index = active_state_session_index(&mut seeded, &session);
        let file = session.files[0].diff.clone();
        let anchor = crate::anchor::comment_anchor_for_file_diff(&file, Some(1), None);
        seeded.comments.push(Comment {
            id: "onboarding-source".into(),
            session_id: Some(seeded.sessions[index].id.clone()),
            path: Some("a.rs".into()),
            line: Some(1),
            anchor,
            body: "Agent explanation".into(),
            state: CommentState::Resolved,
            author: crate::state::Identity::agent(),
            channel: Channel::Onboarding,
            ..Comment::default()
        });
        seeded.save(&watcher.state_path).unwrap();
        session.apply_review_state(seeded.clone());
        let mut baseline = seeded;
        let http = http_state(WebReview::from_session(&session));
        let generation = http.review.read().unwrap().generation;
        let state_path = watcher.state_path.clone();
        let action = CommentAddAction {
            expected_generation: generation,
            path: None,
            line: None,
            end_line: None,
            body: "Please explain the invariant".into(),
            kind: None,
            action: None,
            state: None,
            channel: None,
            source_comment_id: Some("onboarding-source".into()),
        };
        let result = process_action(
            generation,
            ActionCommand::CommentAdd(action),
            &mut session,
            &state_path,
            &mut baseline,
            &mut watcher,
            &http,
        )
        .unwrap();
        assert_eq!(result.result["source_comment_id"], "onboarding-source");
        assert_eq!(result.result["path"], "a.rs");
        assert_eq!(result.result["line"], 1);
        assert_eq!(result.result["channel"], "delegation");
        assert_eq!(result.result["author"]["kind"], "human");
    }

    #[test]
    fn browser_action_contract_has_optimism_busy_rollback_and_text_preservation() {
        for phrase in [
            "expected_generation: generation",
            "pendingActions.has(key)",
            "reportInteraction(\"comment editor\")",
            "rollback()",
            "editorDrafts",
            "generation < returnedGeneration",
        ] {
            assert!(
                COMPONENT_JS.contains(phrase),
                "missing browser contract: {phrase}"
            );
        }
        for action in [
            "file-viewed",
            "skim-acknowledge",
            "comment-add",
            "comment-edit",
            "comment-reply",
            "comment-state",
            "draft-accept",
            "draft-discard",
            "salience-promote",
            "salience-demote",
            "walkthrough-next",
        ] {
            assert!(
                COMPONENT_JS.contains(action),
                "missing browser action {action}"
            );
        }
    }

    #[test]
    fn sse_uses_the_same_host_origin_and_token_guard() {
        // Route-independent validation is intentional: /events and assets
        // cannot bypass the middleware protecting the shell.
        assert_eq!(
            validate_request(
                Some("127.0.0.1:8123"),
                Some("https://attacker.test"),
                Some("token=secret"),
                "127.0.0.1:8123",
                "http://127.0.0.1:8123",
                "secret",
            )
            .unwrap_err()
            .0,
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn fragment_route_uses_the_same_guard_and_rejects_unknown_or_stale_regions() {
        assert!(
            validate_request(
                Some("127.0.0.1:8123"),
                Some("http://127.0.0.1:8123"),
                Some("generation=42&token=secret"),
                "127.0.0.1:8123",
                "http://127.0.0.1:8123",
                "secret",
            )
            .is_ok()
        );
        let review = review_fixture();
        assert_eq!(
            lookup_fragment(&review, "file-core", Some(41))
                .unwrap_err()
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            lookup_fragment(&review, "missing", Some(42)).unwrap_err().0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            lookup_fragment(&review, "file-core", Some(42)).unwrap().id,
            "file-core"
        );
    }

    #[test]
    fn attention_landing_and_peer_full_mode_are_server_rendered_and_escaped() {
        let state = http_state(review_fixture());
        let html = render_shell(&state);
        assert!(html.contains("Attention map"));
        assert!(html.contains("Spotlights"));
        assert!(html.contains("Skim folds"));
        assert!(html.contains("Coverage"));
        assert!(html.contains("Core behavior"));
        assert!(html.contains("Next spotlight"));
        assert!(html.contains("id=\"mode-switch\""));
        assert!(html.contains("Full review"));
        assert!(html.contains("id=\"review-search\""));
        assert!(html.contains("class=\"file-tree full-only\""));
        assert!(html.contains("data-action=\"file-viewed\""));
        assert!(html.contains("generated churn"));
        assert!(!html.contains("version = 99"));
        assert!(html.contains("data-full-loaded=\"false\""));
        let full_fold = render_region(
            state
                .review
                .read()
                .unwrap()
                .region("fold-generated")
                .unwrap(),
            true,
            RenderMode::Full,
        );
        assert!(full_fold.contains("version = 99"));
        assert!(full_fold.contains("data-full-loaded=\"true\""));
        assert!(html.contains("Thread &lt;unsafe&gt;"));
        assert!(html.contains("&lt;script&gt;bad()&lt;/script&gt;"));
        assert!(!html.contains("<script>bad()</script>"));
    }

    #[test]
    fn initial_document_windows_regions_and_keeps_structural_skeletons() {
        let state = http_state(review_fixture());
        let html = render_shell(&state);
        assert!(html.contains("let value = true;"));
        assert!(html.contains("region-skeleton"));
        assert!(html.contains("data-region=\"file-6\""));
        assert!(
            html.len() < 30_000,
            "fixture response unexpectedly grew to {} bytes",
            html.len()
        );
        let fragment = render_region(
            state.review.read().unwrap().region("file-6").unwrap(),
            true,
            RenderMode::Guided,
        );
        assert!(fragment.contains("let value = true;"));
        assert!(fragment.contains("gander-fragment"));
    }

    #[test]
    fn loopback_listener_supports_random_and_pinned_ports() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let random = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let random_address = random.local_addr().unwrap();
            assert_eq!(random_address.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
            assert_ne!(random_address.port(), 0);
            drop(random);

            let pinned = TcpListener::bind((Ipv4Addr::LOCALHOST, random_address.port()))
                .await
                .unwrap();
            assert_eq!(pinned.local_addr().unwrap().port(), random_address.port());
        });
    }

    #[test]
    fn component_css_uses_only_theme_tokens_for_colors() {
        assert_no_literal_colors("component css", COMPONENT_CSS);
        assert!(COMPONENT_CSS.contains("var(--foreground)"));
    }

    #[test]
    fn theme_css_emits_independent_light_and_dark_slots() {
        let css = render_theme_css(&ThemeConfig::default());
        assert!(css.contains("prefers-color-scheme:dark"));
        assert!(css.contains("[data-color-scheme=light]"));
        assert!(css.contains("[data-color-scheme=dark]"));
        assert!(css.contains("--background:rgb("));
        assert!(css.contains("--accent:rgb("));
        assert!(css.contains("--gutter-removed-fg:rgb("));
    }

    #[test]
    fn theme_css_honors_named_palette_and_scheme_overrides() {
        let mut config = ThemeConfig {
            name: "gruvbox".to_owned(),
            ..ThemeConfig::default()
        };
        config.palette.light.background = Some(Rgb::new(1, 2, 3));
        config.palette.dark.background = Some(Rgb::new(4, 5, 6));
        let css = render_theme_css(&config);
        assert!(css.contains("--background:rgb(1 2 3)"));
        assert!(css.contains("--background:rgb(4 5 6)"));
    }

    #[test]
    fn shell_links_token_guarded_assets_and_extra_css_last() {
        let mut state = http_state(review_fixture());
        state.token = Arc::from("secret");
        state.extra_css = Some(Arc::from(".custom{color:var(--accent)}"));
        let html = render_shell(&state);
        let app_css = html
            .find("/assets/app.css?token=secret")
            .expect("real shell should link token-guarded component CSS");
        let extra_css = html
            .find("/assets/extra.css?token=secret")
            .expect("real shell should link token-guarded extra CSS when configured");
        let prepaint = html
            .find(PREPAINT_SCRIPT)
            .expect("real shell should inline the pre-paint theme bootstrap");
        let theme_style = html
            .find("<style>")
            .expect("real shell should inline generated theme token CSS");
        assert!(
            app_css < extra_css,
            "extra CSS must load after component CSS"
        );
        assert!(
            prepaint < theme_style,
            "pre-paint bootstrap must run before theme CSS block"
        );
        assert!(html.contains("data-theme-toggle"));
        assert!(html.contains("id=\"review-stream\""));
    }

    #[test]
    fn theme_control_scripts_contract_stays_tiny_and_handwritten() {
        assert!(PREPAINT_SCRIPT.contains("localStorage.getItem('gander.colorScheme')"));
        assert!(PREPAINT_SCRIPT.contains("document.documentElement.dataset.colorScheme=m"));
        assert!(THEME_CONTROL_SCRIPT.contains("['system','light','dark']"));
        assert!(THEME_CONTROL_SCRIPT.contains("matchMedia('(prefers-color-scheme: dark)')"));
        assert!(THEME_CONTROL_SCRIPT.contains("addEventListener('change'"));
        assert!(THEME_CONTROL_SCRIPT.contains("localStorage.setItem(k,m)"));
        assert_no_literal_colors("inline theme scripts", PREPAINT_SCRIPT);
        assert_no_literal_colors("inline theme scripts", THEME_CONTROL_SCRIPT);
    }

    fn presentation_fixture() -> ReviewSession {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,3 @@\n-old_one\n+new_one\n-old_two\n+new_two\n-old_three\n+new_three\n";
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(raw).unwrap(),
            ReviewState::default(),
        );
        session.syntax.enabled = false;
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let step = |id: &str, line: usize| WalkthroughStep {
            id: id.into(),
            title: Some(id.into()),
            target: crate::attention::target_for_diff(&files, "a.rs", Some(line), None).unwrap(),
            ..WalkthroughStep::default()
        };
        let mut durable = crate::state::ReviewSession {
            id: "web-presentation".into(),
            walkthroughs: vec![Walkthrough {
                id: "tour".into(),
                steps: vec![step("first", 1), step("second", 3)],
                ..Walkthrough::default()
            }],
            ..crate::state::ReviewSession::default()
        };
        durable.target.base = Some(session.target.base.clone());
        durable.target.revision = Some(session.target.rev.clone());
        durable.target.repo = Some(session.canonical_repo().to_owned());
        crate::attention::sync_agent_attention(&mut durable, &files).unwrap();
        session.durable_sessions_mut().push(durable);
        session.stream_mode = true;
        session
    }

    fn presentation_watcher(raw: &str) -> (WebWatcher, tempfile::TempDir, Arc<JjCounts>) {
        let dir = tempfile::tempdir().unwrap();
        let counts = Arc::new(JjCounts::default());
        (
            WebWatcher {
                state_path: dir.path().join("state.json"),
                overlay_path: dir.path().join("agent.json"),
                state_mtime: None,
                overlay_mtime: None,
                repo_fingerprint: None,
                last_repo_poll: None,
                jj: Box::new(WatchJj {
                    counts: counts.clone(),
                    fingerprint: Arc::new(Mutex::new("one".into())),
                    diff: raw.into(),
                }),
                ignore_globs: Vec::new(),
                generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            },
            dir,
            counts,
        )
    }

    #[test]
    fn web_present_methods_match_tui_status_navigation_focus_and_reload_contract() {
        let mut session = presentation_fixture();
        let raw = session
            .files
            .iter()
            .map(|file| file.diff.raw.clone())
            .collect::<String>();
        let (mut watcher, _dir, _) = presentation_watcher(&raw);
        let mut baseline = session.to_state();
        baseline.save(&watcher.state_path).unwrap();
        crate::agent::AgentOverlay::default()
            .save(&watcher.overlay_path)
            .unwrap();
        let interactions = Arc::new(Mutex::new(WebInteractions::default()));
        let mut presentation = None;
        let apply = |command,
                     session: &mut ReviewSession,
                     watcher: &mut WebWatcher,
                     baseline: &mut ReviewState,
                     presentation: &mut Option<WebPresentationState>| {
            apply_web_present_command(
                command,
                session,
                watcher,
                baseline,
                presentation,
                &interactions,
            )
        };

        assert_eq!(
            apply(
                PresentCommand::Status,
                &mut session,
                &mut watcher,
                &mut baseline,
                &mut presentation,
            )
            .unwrap(),
            json!({"active": false})
        );
        let started = apply(
            PresentCommand::Start,
            &mut session,
            &mut watcher,
            &mut baseline,
            &mut presentation,
        )
        .unwrap();
        assert_eq!(started["slide_index"], 0);
        assert_eq!(started["slide_count"], 2);
        assert_eq!(started["view"], "focus");
        assert_eq!(started["current"]["step_id"], "first");
        assert_eq!(started["current"]["part"], 0);
        assert!(started.get("phase").is_none());

        let next = apply(
            PresentCommand::Next,
            &mut session,
            &mut watcher,
            &mut baseline,
            &mut presentation,
        )
        .unwrap();
        assert_eq!(next["current"]["step_id"], "second");
        let previous = apply(
            PresentCommand::Prev,
            &mut session,
            &mut watcher,
            &mut baseline,
            &mut presentation,
        )
        .unwrap();
        assert_eq!(previous["current"]["step_id"], "first");
        assert_eq!(
            apply(
                PresentCommand::GotoIndex(1),
                &mut session,
                &mut watcher,
                &mut baseline,
                &mut presentation
            )
            .unwrap()["slide_index"],
            1
        );
        assert_eq!(
            apply(
                PresentCommand::GotoStep("first".into()),
                &mut session,
                &mut watcher,
                &mut baseline,
                &mut presentation
            )
            .unwrap()["slide_index"],
            0
        );

        let focused = apply(
            PresentCommand::Focus {
                path: "a.rs".into(),
                line: 2,
                end_line: Some(3),
                note: Some("ephemeral".into()),
            },
            &mut session,
            &mut watcher,
            &mut baseline,
            &mut presentation,
        )
        .unwrap();
        assert_eq!(
            focused,
            json!({"ok":true,"path":"a.rs","line":2,"end_line":3})
        );
        assert_eq!(session.focus, Focus::Diff);

        let reloaded = apply(
            PresentCommand::Reload,
            &mut session,
            &mut watcher,
            &mut baseline,
            &mut presentation,
        )
        .unwrap();
        assert!(reloaded["active"].as_bool().unwrap());
        assert_eq!(reloaded["current"]["step_id"], "first");
        assert_eq!(
            apply(
                PresentCommand::End,
                &mut session,
                &mut watcher,
                &mut baseline,
                &mut presentation
            )
            .unwrap(),
            json!({"active":false})
        );
    }

    #[test]
    fn web_present_rejects_inactive_invalid_and_stale_targets_without_moving() {
        let mut session = presentation_fixture();
        let (mut watcher, _dir, _) = presentation_watcher("");
        let mut baseline = session.to_state();
        let interactions = Arc::new(Mutex::new(WebInteractions::default()));
        let mut presentation = None;
        {
            let mut apply = |command| {
                apply_web_present_command(
                    command,
                    &mut session,
                    &mut watcher,
                    &mut baseline,
                    &mut presentation,
                    &interactions,
                )
            };
            assert_eq!(apply(PresentCommand::Next).unwrap_err().0, -32002);
            apply(PresentCommand::Start).unwrap();
            assert_eq!(apply(PresentCommand::GotoIndex(99)).unwrap_err().0, -32602);
            assert_eq!(
                apply(PresentCommand::GotoStep("missing".into()))
                    .unwrap_err()
                    .0,
                -32602
            );
            assert_eq!(
                apply(PresentCommand::Focus {
                    path: "missing.rs".into(),
                    line: 1,
                    end_line: None,
                    note: None
                })
                .unwrap_err()
                .0,
                -32602
            );
            assert_eq!(
                apply(PresentCommand::Focus {
                    path: "a.rs".into(),
                    line: 99,
                    end_line: None,
                    note: None
                })
                .unwrap_err()
                .0,
                -32602
            );
            assert_eq!(
                apply(PresentCommand::Focus {
                    path: "a.rs".into(),
                    line: 3,
                    end_line: Some(2),
                    note: None
                })
                .unwrap_err()
                .0,
                -32602
            );
        }

        let identity = presentation.as_ref().unwrap().identity.clone();
        session.durable_sessions_mut()[0].walkthroughs.clear();
        session.durable_sessions_mut()[0].attention_regions.clear();
        assert!(reconcile_web_presentation(&session, &mut presentation));
        let status = web_present_status(&session, &presentation);
        assert!(status["current"]["stale"].as_bool().unwrap());
        assert_eq!(status["current"]["step_id"], identity.step_id);
        assert_eq!(status["current"]["part"], identity.part);
        assert!(status["current"]["path"].is_null());
        assert_eq!(
            apply_web_present_command(
                PresentCommand::Next,
                &mut session,
                &mut watcher,
                &mut baseline,
                &mut presentation,
                &interactions
            )
            .unwrap_err()
            .0,
            -32002
        );
    }

    #[test]
    fn most_recent_connected_tab_deterministically_controls_busy_and_current_focus() {
        let mut tabs = WebInteractions::default();
        tabs.report(InteractionReport {
            tab_id: "tab-a".into(),
            focus: Some(BrowserFocus {
                path: Some("a.rs".into()),
                old_line: None,
                new_line: Some(1),
                hunk_header: Some("@@ -1,3 +1,3 @@".into()),
                pane: "diff".into(),
            }),
            busy: None,
        })
        .unwrap();
        tabs.report(InteractionReport {
            tab_id: "tab-b".into(),
            focus: Some(BrowserFocus {
                path: Some("a.rs".into()),
                old_line: None,
                new_line: Some(3),
                hunk_header: Some("@@ -1,3 +1,3 @@".into()),
                pane: "files".into(),
            }),
            busy: Some("search".into()),
        })
        .unwrap();
        assert_eq!(tabs.controlling().unwrap().0, "tab-b");
        let interactions = Arc::new(Mutex::new(tabs));
        let mut session = presentation_fixture();
        apply_controlling_focus(&mut session, &interactions);
        assert_eq!(session.focus, Focus::Files);
        assert_eq!(session.selected_line_anchor().unwrap().line(), Some(3));
        let (mut watcher, _dir, _) = presentation_watcher("");
        let mut baseline = session.to_state();
        assert_eq!(
            apply_web_present_command(
                PresentCommand::Status,
                &mut session,
                &mut watcher,
                &mut baseline,
                &mut None,
                &interactions
            )
            .unwrap_err(),
            (-32001, "user is busy: search".into())
        );

        let connection = interactions.lock().unwrap().tabs["tab-b"].connection;
        interactions.lock().unwrap().disconnect("tab-b", connection);
        apply_controlling_focus(&mut session, &interactions);
        assert_eq!(session.focus, Focus::Diff);
        assert_eq!(session.selected_line_anchor().unwrap().line(), Some(1));
        assert!(
            apply_web_present_command(
                PresentCommand::Status,
                &mut session,
                &mut watcher,
                &mut baseline,
                &mut None,
                &interactions
            )
            .is_ok()
        );

        let mut tied = WebInteractions::default();
        tied.tabs.insert(
            "a".into(),
            TabInteraction {
                sequence: 7,
                connection: 1,
                connected: true,
                focus: None,
                busy: None,
            },
        );
        tied.tabs.insert(
            "b".into(),
            TabInteraction {
                sequence: 7,
                connection: 2,
                connected: true,
                focus: None,
                busy: None,
            },
        );
        assert_eq!(tied.controlling().unwrap().0, "b");
    }

    #[test]
    fn presenter_watch_broadcasts_to_all_tabs_and_coalesces_bursts() {
        let (sender, receiver_a) = tokio::sync::watch::channel::<Option<Arc<PresentEvent>>>(None);
        let receiver_b = receiver_a.clone();
        let event = |sequence| {
            Arc::new(PresentEvent {
                sequence,
                command: "goto",
                status: json!({"active":true}),
                target: None,
                note: None,
            })
        };
        sender.send_replace(Some(event(1)));
        sender.send_replace(Some(event(2)));
        sender.send_replace(Some(event(3)));
        assert_eq!(receiver_a.borrow().as_ref().unwrap().sequence, 3);
        assert_eq!(receiver_b.borrow().as_ref().unwrap().sequence, 3);
        let wire = sse_present_event(receiver_a.borrow().as_ref().unwrap());
        assert!(format!("{wire:?}").contains("present"));
    }

    #[test]
    fn browser_presentation_source_contract_covers_follow_human_priority_and_reduced_motion() {
        for needle in [
            "pauseFollow",
            "rejoinFollow",
            "presenter-edge",
            "ganderPendingPresent",
            "requestAnimationFrame",
            "prefers-reduced-motion: reduce",
            "behavior: reducedMotion.matches ? \"auto\" : \"smooth\"",
            "reportInteraction(\"search\")",
            "tab_id: tabId",
        ] {
            assert!(
                COMPONENT_JS.contains(needle),
                "missing browser contract: {needle}"
            );
        }
        assert!(COMPONENT_CSS.contains(".present-target-static"));
        assert!(COMPONENT_CSS.contains("@media (prefers-reduced-motion: reduce)"));
        assert!(COMPONENT_CSS.contains(".present-expanded"));
        assert!(render_shell(&http_state(review_fixture())).contains("Following paused — rejoin"));
    }

    #[test]
    fn interaction_reports_are_bounded_validated_and_disconnect_cleanly() {
        let mut tabs = WebInteractions::default();
        assert!(
            tabs.report(InteractionReport {
                tab_id: "bad tab".into(),
                focus: None,
                busy: None
            })
            .is_err()
        );
        assert!(
            tabs.report(InteractionReport {
                tab_id: "ok".into(),
                focus: None,
                busy: Some("privileged-action".into())
            })
            .is_err()
        );
        let old_connection = tabs.connect("ok").unwrap();
        assert_eq!(tabs.tabs.len(), 1);
        let new_connection = tabs.connect("ok").unwrap();
        tabs.disconnect("ok", old_connection);
        assert_eq!(
            tabs.tabs.len(),
            1,
            "an old SSE task cannot remove a reconnect"
        );
        tabs.disconnect("ok", new_connection);
        assert!(tabs.tabs.is_empty());
        assert!(valid_tab_id("550e8400-e29b-41d4-a716-446655440000"));
        assert!(!valid_tab_id(&"x".repeat(129)));
    }

    #[test]
    fn extra_css_read_error_names_config_key_and_path() {
        let path = std::path::Path::new("/definitely/missing/gander-extra.css");
        let error = read_extra_css(path).unwrap_err().to_string();
        assert!(error.contains("[web] extra-css"));
        assert!(error.contains("/definitely/missing/gander-extra.css"));
    }

    fn assert_no_literal_colors(label: &str, css_or_script: &str) {
        for needle in ["#", "rgb(", "rgba(", "hsl(", "hsla("] {
            assert!(
                !css_or_script.contains(needle),
                "{label} contains literal color marker {needle}"
            );
        }
    }
}
