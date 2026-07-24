//! Standalone loopback web peer (M16 phase 1).
//!
//! Phase 1 deliberately serves only a server-rendered lifecycle shell. The
//! process nevertheless owns the complete peer-instance lifecycle now: the
//! existing registry entry, ACP socket/typed request loop, strict HTTP guard,
//! and graceful cleanup. Stream projection, browser presentation, and review
//! actions remain later phases.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use chrono::Utc;
use color_eyre::eyre::{Context, Result};
use serde_json::{Value, json};
use tokio::net::TcpListener;

use crate::{
    acp::socket::{AcpBridge, PresentCommand},
    app::{Focus, ReviewSession},
    config::ThemeConfig,
    jj::JjBackend,
    registry::{InstanceInfo, InstanceRegistration},
    state::{ReviewState, ReviewStateTombstones},
    theme::{Rgb, ThemeKind, ThemeSlots},
};

const COMPONENT_CSS: &str = include_str!("web.css");

pub(crate) struct WebParams {
    pub session: ReviewSession,
    pub overlay_path: PathBuf,
    pub state_path: PathBuf,
    pub socket_path: PathBuf,
    pub registry_dir: PathBuf,
    pub workspace_root: PathBuf,
    pub acp_jj: Box<dyn JjBackend + Send>,
    pub port: u16,
    pub no_open: bool,
    pub theme: ThemeConfig,
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
    registration: Arc<Mutex<InstanceRegistration>>,
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
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    loop {
        tokio::select! {
            result = &mut server => {
                result.wrap_err("web server failed")?;
                break;
            }
            _ = tick.tick() => {
                process_acp_requests(
                    &mut bridge,
                    &mut params.session,
                    &params.state_path,
                    &mut baseline,
                );
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
    let token = state.token.as_ref();
    Html(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Gander review</title><style>{}</style><link rel=\"stylesheet\" href=\"/assets/app.css?token={}\"></head><body><main><p class=\"eyebrow\">Gander local review</p><section class=\"shell\"><h1>Web review peer is running</h1><p>{}</p><p class=\"meta\">Target <code>{}</code></p><p>Phase 1 validates the secure HTTP, registry, and ACP lifecycle. The shared review stream arrives in Phase 2.</p></section></main></body></html>",
        state.theme_css,
        html_escape(token),
        html_escape(&state.summary),
        html_escape(&state.target),
    ))
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        COMPONENT_CSS,
    )
}

async fn events() -> impl IntoResponse {
    let body = "event: notice\ndata: {\"phase\":1,\"status\":\"ready\"}\n\n";
    (
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CONNECTION, "keep-alive"),
        ],
        Body::from(body),
    )
}

async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "not found")
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
        ":root{{{}}}@media(prefers-color-scheme:dark){{:root{{{}}}}}",
        slot_tokens(light_palette.background, light),
        slot_tokens(dark_palette.background, dark),
    )
}

fn slot_tokens(background: Rgb, slots: ThemeSlots) -> String {
    format!(
        "--background:{};--foreground:{};--muted:{};--subtle:{};--accent:{};--surface:{};",
        css_rgb(background),
        css_rgb(slots.foreground),
        css_rgb(slots.muted),
        css_rgb(slots.subtle),
        css_rgb(slots.accent),
        css_rgb(slots.selection_bg),
    )
}

fn css_rgb(rgb: Rgb) -> String {
    format!("rgb({} {} {})", rgb.r, rgb.g, rgb.b)
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!COMPONENT_CSS.contains('#'));
        assert!(!COMPONENT_CSS.contains("rgb("));
        assert!(COMPONENT_CSS.contains("var(--foreground)"));
    }

    #[test]
    fn theme_css_emits_independent_light_and_dark_slots() {
        let css = render_theme_css(&ThemeConfig::default());
        assert!(css.contains("prefers-color-scheme:dark"));
        assert!(css.contains("--background:rgb("));
        assert!(css.contains("--accent:rgb("));
    }
}
