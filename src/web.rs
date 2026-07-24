//! Standalone loopback web peer.
//!
//! The browser is a renderer over the app-owned reading projection. This
//! phase is read-only from the browser: durable state and jj changes are
//! projected into surgical SSE patches. Presentation broadcast and review
//! mutations intentionally remain outside the HTTP surface.

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
    extract::{Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{
        Html, IntoResponse, Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::get,
};
use chrono::Utc;
use color_eyre::eyre::{Context, Result};
use futures_core::Stream;
use serde_json::{Value, json};
use tokio::net::TcpListener;

use crate::{
    acp::socket::{AcpBridge, PresentCommand},
    app::{
        DiffRowKind, Focus, ReadingAnnotation, ReadingAnnotationSource, ReadingProjection,
        ReadingRegion, ReadingRegionKind, ReviewSession,
    },
    config::ThemeConfig,
    diff::DiffSet,
    generated::GeneratedMatcher,
    jj::JjBackend,
    registry::{InstanceInfo, InstanceRegistration},
    state::{AuthorKind, Channel, ReviewState, ReviewStateTombstones, Salience},
    theme::{Rgb, ThemeKind, ThemeSlots},
};

const COMPONENT_CSS: &str = include_str!("web.css");
const COMPONENT_JS: &str = include_str!("web.js");
const INITIAL_REGION_WINDOW: usize = 4;
const WATCH_TICK: Duration = Duration::from_millis(250);
const REPO_POLL_INTERVAL: Duration = Duration::from_secs(2);
const SSE_KEEPALIVE: Duration = Duration::from_secs(15);
const SSE_BROADCAST_CAPACITY: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
enum RenderMode {
    Guided,
    Full,
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
    shutdown: tokio::sync::watch::Receiver<bool>,
    extra_css: Option<Arc<str>>,
    registration: Arc<Mutex<InstanceRegistration>>,
}

#[derive(Debug, Clone)]
struct WebReview {
    projection: ReadingProjection,
    files: Vec<WebFile>,
    generation: u64,
    target: String,
}

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

#[derive(Debug, Clone)]
struct WebFile {
    path: String,
    additions: usize,
    deletions: usize,
    viewed: bool,
    generated: bool,
    region_id: Option<String>,
}

impl WebReview {
    fn from_session(session: &ReviewSession) -> Self {
        let projection = session.reading_projection();
        let files = session
            .files
            .iter()
            .map(|file| WebFile {
                path: file.path.clone(),
                additions: file.additions,
                deletions: file.deletions,
                viewed: file.viewed || file.caught_up,
                generated: file.generated,
                region_id: projection.file_region_ids.get(&file.path).cloned(),
            })
            .collect();
        Self {
            projection,
            files,
            generation: session.stream_inputs_generation(),
            target: session.target.to_string(),
        }
    }

    fn region(&self, id: &str) -> Option<&ReadingRegion> {
        self.projection
            .regions
            .iter()
            .find(|region| region.id == id)
    }
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
        shutdown: stream_shutdown_rx,
        extra_css: params
            .extra_css
            .as_deref()
            .map(read_extra_css)
            .transpose()?
            .map(Arc::from),
        registration: Arc::new(Mutex::new(registration)),
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
                );
                watcher.poll_files(&mut params.session, &mut baseline);
                watcher.poll_repo(&mut params.session);
                if params.session.stream_inputs_generation() != before {
                    publish_projection(&http_state, &params.session);
                }
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

    if let Ok(mut registration) = state.registration.lock() {
        let _ = registration.record_input(&state.base, &state.rev, &state.summary);
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
    let mut receiver = state.events.subscribe();
    let (sender, body_receiver) = tokio::sync::mpsc::channel(1);
    let state_for_stream = state.clone();
    let mut shutdown = state.shutdown.clone();
    tokio::spawn(async move {
        let current = full_projection_event(&state_for_stream);
        if client_generation != Some(current.generation)
            && sender.send(sse_state_event(&current)).await.is_err()
        {
            return;
        }
        loop {
            let received = tokio::select! {
                _ = shutdown.changed() => break,
                received = receiver.recv() => received,
            };
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
    out.push_str("</strong></div><div class=\"controls\"><label class=\"search\">Search <input id=\"review-search\" type=\"search\" placeholder=\"File, code, or comment\"></label><button class=\"theme-toggle\" type=\"button\" data-theme-toggle aria-label=\"Cycle color scheme\">Theme: <span data-theme-label>system</span></button><button id=\"mode-switch\" type=\"button\" aria-pressed=\"false\">Full review</button></div></header><div class=\"app-layout\">");
    out.push_str(&render_file_tree_html(review));
    out.push_str("<main>");
    out.push_str(&render_overview_with_target(review, &state.target));
    out.push_str("<section id=\"review-stream\" class=\"review-stream\" aria-label=\"Shared review stream\"><div class=\"stream-heading\"><div><p class=\"eyebrow\">Shared projection</p><h2>Review stream</h2></div><p class=\"guided-only\">Skims stay compact; spotlights carry narration.</p><p class=\"full-only\">Every file and line is visible. Salience remains in the margin.</p></div>");
    for (index, region) in projection.regions.iter().enumerate() {
        if index < INITIAL_REGION_WINDOW || matches!(region.kind, ReadingRegionKind::Chapter(_)) {
            out.push_str(&render_region(region, false, RenderMode::Guided));
        } else {
            out.push_str("<section id=\"");
            escape_to(&mut out, &region.id);
            out.push_str("\" class=\"region region-skeleton\" data-region=\"");
            escape_to(&mut out, &region.id);
            out.push_str("\"><div class=\"skeleton-label\"><strong>");
            escape_to(&mut out, &region_label(region));
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

fn metric(out: &mut String, label: &str, value: usize, detail: &str) {
    out.push_str("<div class=\"metric\"><span>");
    escape_to(out, label);
    out.push_str("</span><strong>");
    out.push_str(&value.to_string());
    out.push_str("</strong><small>");
    escape_to(out, detail);
    out.push_str("</small></div>");
}

fn render_overview(review: &WebReview) -> String {
    render_overview_with_target(review, &review.target)
}

fn render_overview_with_target(review: &WebReview, target: &str) -> String {
    let projection = &review.projection;
    let mut out = String::from(
        "<section id=\"overview\" data-patch-id=\"overview\" class=\"attention-map\" aria-labelledby=\"attention-title\"><p class=\"eyebrow\">Attention map</p><h1 id=\"attention-title\">",
    );
    escape_to(&mut out, &projection.summary);
    out.push_str("</h1><p class=\"meta\">Target <code>");
    escape_to(&mut out, target);
    out.push_str("</code></p><div class=\"metrics\">");
    metric(
        &mut out,
        "Spotlights",
        projection.spotlight_count,
        "curated stops",
    );
    metric(
        &mut out,
        "Skim folds",
        projection.skim_count,
        &format!("{} files", projection.skim_files),
    );
    out.push_str(&render_coverage(review));
    metric(
        &mut out,
        "Files",
        review.files.len(),
        &format!(
            "{} viewed",
            review.files.iter().filter(|file| file.viewed).count()
        ),
    );
    out.push_str("</div>");
    if !projection.chapters.is_empty() {
        out.push_str(
            "<nav class=\"chapters\" aria-label=\"Review chapters\"><h2>Chapters</h2><ol>",
        );
        for (index, chapter) in projection.chapters.iter().enumerate() {
            out.push_str("<li><a href=\"#");
            if let Some(region) = projection.regions.iter().find(|region| matches!(&region.kind, ReadingRegionKind::Chapter(candidate) if candidate == chapter)) {
                escape_to(&mut out, &region.id);
            }
            out.push_str("\"><span>");
            out.push_str(&(index + 1).to_string());
            out.push_str("</span> ");
            escape_to(
                &mut out,
                chapter
                    .description
                    .lines()
                    .next()
                    .unwrap_or(&chapter.change_id),
            );
            out.push_str("</a></li>");
        }
        out.push_str("</ol></nav>");
    }
    if projection.has_walkthrough {
        out.push_str("<a class=\"primary-action\" href=\"#review-stream\">Start guided tour</a>");
    }
    out.push_str("</section>");
    out
}

fn render_coverage(review: &WebReview) -> String {
    let mut out = String::from(
        "<div id=\"coverage\" data-patch-id=\"coverage\" class=\"metric\"><span>Coverage</span><strong>",
    );
    out.push_str(&review.projection.coverage.covered.to_string());
    out.push_str("</strong><small>of ");
    out.push_str(&review.projection.coverage.total.to_string());
    out.push_str(" attention units</small></div>");
    out
}

fn render_footer(review: &WebReview) -> String {
    format!(
        "<footer id=\"footer\" data-patch-id=\"footer\" class=\"meta\">Generation {} · coverage {}/{}</footer>",
        review.generation, review.projection.coverage.covered, review.projection.coverage.total
    )
}

fn render_file_tree_html(review: &WebReview) -> String {
    let mut out = String::from(
        "<aside id=\"file-tree\" data-patch-id=\"file-tree\" class=\"file-tree full-only\" aria-label=\"Files\"><h2>Files</h2><p class=\"meta\">Traditional review</p><ul>",
    );
    for file in &review.files {
        out.push_str("<li data-search=\"");
        escape_to(&mut out, &file.path.to_lowercase());
        out.push_str("\"><label><input type=\"checkbox\" disabled ");
        if file.viewed {
            out.push_str("checked ");
        }
        out.push_str("aria-label=\"Viewed: ");
        escape_to(&mut out, &file.path);
        out.push_str("\"><a href=\"#");
        if let Some(region) = &file.region_id {
            escape_to(&mut out, region);
        }
        out.push_str("\">");
        escape_to(&mut out, &file.path);
        out.push_str("</a></label><small>+");
        out.push_str(&file.additions.to_string());
        out.push_str(" −");
        out.push_str(&file.deletions.to_string());
        if file.generated {
            out.push_str(" · generated");
        }
        out.push_str("</small></li>");
    }
    out.push_str("</ul></aside>");
    out
}

fn render_region(region: &ReadingRegion, fragment: bool, mode: RenderMode) -> String {
    let mut out = String::new();
    out.push_str("<section id=\"");
    escape_to(&mut out, &region.id);
    out.push_str("\" class=\"region ");
    out.push_str(match region.kind {
        ReadingRegionKind::Chapter(_) => "chapter-region",
        ReadingRegionKind::File { .. } => "file-region",
        ReadingRegionKind::Skim(_) => "skim-region",
    });
    out.push_str("\" data-region=\"");
    escape_to(&mut out, &region.id);
    if matches!(region.kind, ReadingRegionKind::Skim(_)) {
        out.push_str("\" data-full-loaded=\"");
        out.push_str(if mode == RenderMode::Full {
            "true"
        } else {
            "false"
        });
    }
    out.push_str("\" data-search=\"");
    escape_to(&mut out, &region_search_text(region, mode));
    out.push_str("\">");
    match &region.kind {
        ReadingRegionKind::Chapter(chapter) => {
            out.push_str("<header class=\"chapter\"><p class=\"eyebrow\">Chapter</p><h2>");
            escape_to(
                &mut out,
                chapter
                    .description
                    .lines()
                    .next()
                    .unwrap_or(&chapter.change_id),
            );
            out.push_str("</h2><p><code>");
            escape_to(&mut out, &chapter.change_id);
            out.push_str("</code>");
            if !chapter.bookmarks.is_empty() {
                out.push_str(" · ");
                escape_to(&mut out, &chapter.bookmarks);
            }
            out.push_str(" · +");
            out.push_str(&chapter.additions.to_string());
            out.push_str(" −");
            out.push_str(&chapter.deletions.to_string());
            out.push_str("</p></header>");
        }
        ReadingRegionKind::File { path, .. } => {
            out.push_str("<header class=\"file-header\"><h3>");
            escape_to(&mut out, path);
            out.push_str("</h3></header><div class=\"diff-table\">");
            for row in &region.rows {
                render_diff_row(&mut out, row);
            }
            out.push_str("</div>");
        }
        ReadingRegionKind::Skim(fold) => {
            out.push_str(
                "<div class=\"guided-only skim-fold\"><span aria-hidden=\"true\">⌄</span><strong>",
            );
            out.push_str(&fold.files.len().to_string());
            out.push_str(if fold.files.len() == 1 {
                " file"
            } else {
                " files"
            });
            out.push_str("</strong><span>");
            escape_to(&mut out, &fold.rationale);
            out.push_str("</span><small>+");
            out.push_str(&fold.additions.to_string());
            out.push_str(" −");
            out.push_str(&fold.deletions.to_string());
            if fold.acknowledged {
                out.push_str(" · ✓ acknowledged");
            }
            out.push_str("</small></div><div class=\"full-only\">");
            if mode == RenderMode::Full {
                out.push_str("<header class=\"file-header\"><h3>");
                escape_to(&mut out, &fold.files.join(", "));
                out.push_str(
                    "</h3><span class=\"salience-chip\">skim</span></header><div class=\"diff-table\">",
                );
                for row in region.rows.iter().skip(1) {
                    render_diff_row(&mut out, row);
                }
                out.push_str("</div>");
            } else {
                out.push_str(
                    "<p class=\"fragment-loading\">Loading every line for full review…</p>",
                );
            }
            out.push_str("</div>");
            for annotation in &region.rows[0].annotations {
                render_annotation(&mut out, annotation);
            }
        }
    }
    if fragment {
        out.push_str("<!-- gander-fragment -->");
    }
    out.push_str("</section>");
    out
}

fn render_diff_row(out: &mut String, row: &crate::app::ReadingRow) {
    let Some(diff) = &row.diff else {
        for annotation in &row.annotations {
            render_annotation(out, annotation);
        }
        return;
    };
    let (kind, label) = match diff.kind {
        DiffRowKind::DiffLine(crate::diff::DiffLineKind::Added) => ("added", "+"),
        DiffRowKind::DiffLine(crate::diff::DiffLineKind::Removed) => ("removed", "−"),
        DiffRowKind::DiffLine(crate::diff::DiffLineKind::Context) => ("context", " "),
        DiffRowKind::DiffLine(crate::diff::DiffLineKind::Meta) => ("meta", "\\"),
        DiffRowKind::HunkHeader => ("hunk", "@@"),
        DiffRowKind::FileHeader => ("file-title", ""),
        DiffRowKind::Placeholder => ("placeholder", "…"),
        _ => ("structural", diff.prefix),
    };
    out.push_str("<div class=\"diff-row ");
    out.push_str(kind);
    if let Some(salience) = row.salience {
        out.push_str(" salience-");
        out.push_str(salience_label(salience));
    }
    out.push_str("\" id=\"");
    escape_to(out, &dom_row_id(&row.id));
    out.push_str("\" data-path=\"");
    escape_to(out, row.path.as_deref().unwrap_or_default());
    out.push_str("\"><span class=\"salience-margin\" aria-label=\"");
    escape_to(
        out,
        row.salience.map(salience_label).unwrap_or("structural"),
    );
    out.push_str("\"></span><span class=\"line-number old\">");
    if let Some(line) = diff.old_lineno {
        out.push_str(&line.to_string());
    }
    out.push_str("</span><span class=\"line-number new\">");
    if let Some(line) = diff.new_lineno {
        out.push_str(&line.to_string());
    }
    out.push_str("</span><span class=\"prefix\">");
    escape_to(out, label);
    out.push_str("</span><code>");
    escape_to(out, &diff.text);
    out.push_str("</code></div>");
    for annotation in &row.annotations {
        render_annotation(out, annotation);
    }
}

fn render_annotation(out: &mut String, annotation: &ReadingAnnotation) {
    out.push_str("<article class=\"annotation channel-");
    out.push_str(channel_label(annotation.channel()));
    out.push_str("\" data-owner-row=\"");
    escape_to(out, &annotation.owner_row_id);
    out.push_str("\"><header><span class=\"channel\">→ ");
    escape_to(out, annotation.channel().audience_label());
    out.push_str("</span>");
    match &annotation.source {
        ReadingAnnotationSource::Comment(comment) => {
            out.push_str("<span>");
            escape_to(out, author_label(comment.author.kind));
            out.push(':');
            escape_to(out, &comment.author.name);
            out.push_str("</span><span class=\"badge\">");
            escape_to(out, comment.state.label());
            out.push_str("</span></header><h4>");
            let title = comment
                .body
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("(empty comment)");
            escape_to(out, title.trim());
            out.push_str("</h4><p>");
            escape_to(out, &comment.body);
            out.push_str("</p>");
            for reply in &comment.replies {
                out.push_str("<div class=\"reply\"><strong>");
                escape_to(out, author_label(reply.author.kind));
                out.push(':');
                escape_to(out, &reply.author.name);
                out.push_str("</strong><p>");
                escape_to(out, &reply.body);
                out.push_str("</p></div>");
            }
        }
        ReadingAnnotationSource::Walkthrough {
            step,
            target,
            part,
            rationale,
        } => {
            out.push_str("<span>");
            if let Some(author) = &step.author {
                escape_to(out, author_label(author.kind));
                out.push(':');
                escape_to(out, &author.name);
            } else {
                out.push_str("walkthrough");
            }
            out.push_str("</span><span class=\"badge\">spotlight ");
            out.push_str(&(part + 1).to_string());
            out.push_str("</span></header><h4>");
            escape_to(out, step.title.as_deref().unwrap_or("Walkthrough step"));
            out.push_str("</h4><p class=\"target\">");
            escape_to(out, &target_label(target));
            out.push_str("</p>");
            if let Some(why) = step.why.as_deref().filter(|value| !value.trim().is_empty()) {
                out.push_str("<p><strong>Why:</strong> ");
                escape_to(out, why);
                out.push_str("</p>");
            }
            if let Some(rationale) = rationale
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                out.push_str("<p><strong>Rationale:</strong> ");
                escape_to(out, rationale);
                out.push_str("</p>");
            }
            if let Some(body) = step
                .body
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                out.push_str("<p>");
                escape_to(out, body);
                out.push_str("</p>");
            }
            for artifact in &step.artifacts {
                out.push_str("<details><summary>");
                escape_to(out, &artifact.title);
                out.push_str("</summary><pre>");
                escape_to(out, &artifact.body);
                out.push_str("</pre></details>");
            }
        }
    }
    out.push_str("</article>");
}

fn region_label(region: &ReadingRegion) -> String {
    match &region.kind {
        ReadingRegionKind::Chapter(chapter) => format!("Chapter {}", chapter.change_id),
        ReadingRegionKind::File { path, .. } => path.clone(),
        ReadingRegionKind::Skim(fold) => format!("Skim: {}", fold.rationale),
    }
}

fn region_search_text(region: &ReadingRegion, mode: RenderMode) -> String {
    let mut text = region_label(region).to_lowercase();
    let visible_rows =
        if mode == RenderMode::Guided && matches!(region.kind, ReadingRegionKind::Skim(_)) {
            &region.rows[..region.rows.len().min(1)]
        } else {
            &region.rows
        };
    for row in visible_rows {
        if let Some(diff) = &row.diff {
            text.push(' ');
            text.push_str(&diff.text.to_lowercase());
        }
        for annotation in &row.annotations {
            match &annotation.source {
                ReadingAnnotationSource::Comment(comment) => {
                    text.push(' ');
                    text.push_str(&comment.body.to_lowercase());
                }
                ReadingAnnotationSource::Walkthrough { step, .. } => {
                    text.push(' ');
                    text.push_str(&step.title.as_deref().unwrap_or_default().to_lowercase());
                    text.push(' ');
                    text.push_str(&step.body.as_deref().unwrap_or_default().to_lowercase());
                }
            }
        }
    }
    text
}

fn target_label(target: &crate::state::ReviewTarget) -> String {
    let mut label = target
        .file
        .clone()
        .unwrap_or_else(|| "review target".into());
    if let Some(line) = target.line {
        label.push(':');
        label.push_str(&line.to_string());
        if let Some(end) = target.end_line.filter(|end| *end != line) {
            label.push('-');
            label.push_str(&end.to_string());
        }
    }
    label
}

fn salience_label(salience: Salience) -> &'static str {
    match salience {
        Salience::Spotlight => "spotlight",
        Salience::Supporting => "supporting",
        Salience::Skim => "skim",
    }
}

fn channel_label(channel: Channel) -> &'static str {
    match channel {
        Channel::Onboarding => "onboarding",
        Channel::Delegation => "delegation",
        Channel::Collaboration => "collaboration",
        Channel::Note => "note",
    }
}

fn author_label(kind: AuthorKind) -> &'static str {
    match kind {
        AuthorKind::Human => "human",
        AuthorKind::Agent => "agent",
    }
}

fn dom_row_id(value: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"gander-web-row-v1\0");
    hash.update(value.as_bytes());
    format!("row-{:x}", hash.finalize())
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

fn process_acp_requests(
    bridge: &mut AcpBridge,
    session: &mut ReviewSession,
    state_path: &std::path::Path,
    baseline: &mut ReviewState,
) {
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
        request.respond(apply_present_skeleton(command, session));
    }
}

fn apply_present_skeleton(
    command: PresentCommand,
    session: &mut ReviewSession,
) -> std::result::Result<Value, (i64, String)> {
    match command {
        PresentCommand::Focus {
            path,
            line,
            end_line,
            ..
        } => {
            if !session.files.iter().any(|file| file.path == path) {
                return Err((-32602, format!("path is not in the diff: {path}")));
            }
            let requested_end = end_line.unwrap_or(line);
            let row = session
                .review_stream()
                .rows
                .iter()
                .enumerate()
                .find_map(|(index, row)| {
                    (row.path.as_deref() == Some(path.as_str())
                        && row
                            .anchor
                            .as_ref()
                            .and_then(crate::anchor::CommentAnchor::line)
                            .is_some_and(|anchor| line <= anchor && anchor <= requested_end))
                    .then_some(index)
                });
            let Some(row) = row else {
                return Err((
                    -32602,
                    format!("location is not in the diff: {path}:{line}"),
                ));
            };
            session.select_stream_row(row, true);
            session.focus = Focus::Diff;
            Ok(json!({ "ok": true, "path": path, "line": line, "end_line": end_line, "phase": 1 }))
        }
        PresentCommand::Reload => Ok(json!({ "active": false, "phase": 1, "reloaded": true })),
        PresentCommand::Status
        | PresentCommand::Start
        | PresentCommand::End
        | PresentCommand::Next
        | PresentCommand::Prev
        | PresentCommand::GotoIndex(_)
        | PresentCommand::GotoStep(_) => Ok(json!({ "active": false, "phase": 1 })),
    }
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
    let light_palette = config.base_palette(ThemeKind::Light);
    let dark_palette = config.base_palette(ThemeKind::Dark);
    let light = ThemeSlots::derive(light_palette, light_palette.background);
    let dark = ThemeSlots::derive(dark_palette, dark_palette.background);
    format!(
        ":root,[data-color-scheme=light]{{color-scheme:light;{}}}@media(prefers-color-scheme:dark){{:root{{color-scheme:dark;{}}}}}[data-color-scheme=dark]{{color-scheme:dark;{}}}",
        slot_tokens(light_palette.background, light),
        slot_tokens(dark_palette.background, dark),
        slot_tokens(dark_palette.background, dark),
    )
}

fn slot_tokens(background: Rgb, slots: ThemeSlots) -> String {
    format!(
        "--background:{};--foreground:{};--muted:{};--subtle:{};--accent:{};--warning:{};--info:{};--detail:{};--secondary:{};--positive:{};--negative:{};--surface:{};--range-bg:{};--added-line-bg:{};--removed-line-bg:{};--shadow:{};--added-word-fg:{};--added-word-bg:{};--removed-word-fg:{};--removed-word-bg:{};--gutter-added-fg:{};--gutter-removed-fg:{};",
        css_rgb(background),
        css_rgb(slots.foreground),
        css_rgb(slots.muted),
        css_rgb(slots.subtle),
        css_rgb(slots.accent),
        css_rgb(slots.warning),
        css_rgb(slots.info),
        css_rgb(slots.detail),
        css_rgb(slots.secondary),
        css_rgb(slots.positive),
        css_rgb(slots.negative),
        css_rgb(slots.selection_bg),
        css_rgb(slots.range_bg),
        css_rgb(slots.added_line_bg),
        css_rgb(slots.removed_line_bg),
        css_rgb(slots.range_bg),
        css_rgb(slots.added_word_fg),
        css_rgb(slots.added_word_bg),
        css_rgb(slots.removed_word_fg),
        css_rgb(slots.removed_word_bg),
        css_rgb(slots.gutter_added_fg),
        css_rgb(slots.gutter_removed_fg),
    )
}

fn read_extra_css(path: &std::path::Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| {
        format!(
            "failed to read [web] extra-css stylesheet at {}",
            path.display()
        )
    })
}

const PREPAINT_SCRIPT: &str = "(()=>{try{let m=localStorage.getItem('gander.colorScheme');if(m==='light'||m==='dark')document.documentElement.dataset.colorScheme=m;}catch(e){}})();";
const THEME_CONTROL_SCRIPT: &str = "(()=>{let k='gander.colorScheme',o=['system','light','dark'],q=matchMedia('(prefers-color-scheme: dark)'),b=document.querySelector('[data-theme-toggle]'),l=document.querySelector('[data-theme-label]');function g(){try{return localStorage.getItem(k)||'system'}catch(e){return 'system'}}function s(m){document.documentElement.dataset.colorScheme=(m==='light'||m==='dark')?m:'';if(l)l.textContent=m}function set(m){try{m==='system'?localStorage.removeItem(k):localStorage.setItem(k,m)}catch(e){}s(m)}if(b)b.addEventListener('click',()=>set(o[(o.indexOf(g())+1)%o.length]));q.addEventListener&&q.addEventListener('change',()=>{if(g()==='system')s('system')});s(g())})();";

fn css_rgb(rgb: Rgb) -> String {
    format!("rgb({} {} {})", rgb.r, rgb.g, rgb.b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{ChapterHeader, Coverage, DiffRow, ReadingRow, SkimFold},
        diff::DiffLineKind,
        jj::{JjChangeSummary, JjOperationSummary, ReviewTarget, TargetAuthor},
        state::{AttentionProgressTarget, Comment},
    };
    use std::{
        path::Path as FsPath,
        sync::atomic::{AtomicUsize, Ordering},
    };

    fn row(id: &str, path: &str, text: &str, salience: Salience) -> ReadingRow {
        ReadingRow {
            id: id.into(),
            path: Some(path.into()),
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
            .map(|(index, (path, region))| WebFile {
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
            shutdown: tokio::sync::watch::channel(false).1,
            extra_css: None,
            registration: Arc::new(Mutex::new(registration)),
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
        assert!(html.contains("Start guided tour"));
        assert!(html.contains("id=\"mode-switch\""));
        assert!(html.contains("Full review"));
        assert!(html.contains("id=\"review-search\""));
        assert!(html.contains("class=\"file-tree full-only\""));
        assert!(html.contains("type=\"checkbox\" disabled"));
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
