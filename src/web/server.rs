use super::*;

pub(super) fn router(state: HttpState) -> Router {
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
        .route("/actions/walkthrough-start", post(walkthrough_start))
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

pub(super) async fn security_guard(
    State(state): State<HttpState>,
    request: Request,
    next: Next,
) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    let origin = match request.headers().get(header::ORIGIN) {
        Some(value) => match value.to_str() {
            Ok(value) => Some(value),
            Err(_) => {
                return apply_security_headers(
                    (StatusCode::FORBIDDEN, "invalid Origin header").into_response(),
                    &state.content_security_policy,
                );
            }
        },
        None => None,
    };
    let response = if !state.worker_healthy.load(Ordering::Acquire) {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "web coordinator unavailable",
        )
            .into_response()
    } else if let Err((status, message)) = validate_request(
        host,
        origin,
        request.uri().query(),
        &state.expected_host,
        &state.expected_origin,
        &state.token,
    ) {
        (status, message).into_response()
    } else {
        next.run(request).await
    };
    apply_security_headers(response, &state.content_security_policy)
}

pub(super) fn apply_security_headers(
    mut response: Response,
    content_security_policy: &str,
) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
        .headers_mut()
        .insert("x-frame-options", HeaderValue::from_static("DENY"));
    if let Ok(value) = HeaderValue::from_str(content_security_policy) {
        response
            .headers_mut()
            .insert("content-security-policy", value);
    }
    response
}

pub(super) fn live_content_security_policy(style_nonce: &str) -> String {
    format!(
        "default-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'; object-src 'none'; font-src 'none'; img-src 'self' data:; connect-src 'self'; style-src 'self' 'nonce-{style_nonce}'; script-src 'self' {} {}",
        script_hash_source(PREPAINT_SCRIPT),
        script_hash_source(THEME_CONTROL_SCRIPT),
    )
}

pub(super) fn validate_request(
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

pub(super) fn constant_time_eq(candidate: &str, expected: &str) -> bool {
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

pub(super) fn request_token(query: Option<&str>) -> Option<&str> {
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

pub(super) async fn shell(State(state): State<HttpState>) -> Response {
    match tokio::task::spawn_blocking(move || render_shell(&state)).await {
        Ok(html) => Html(html).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "web render worker unavailable",
        )
            .into_response(),
    }
}

pub(super) async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        COMPONENT_CSS,
    )
}

pub(super) async fn script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        COMPONENT_JS,
    )
}

pub(super) async fn fragment(
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
    fragment_response(state, region, requested_generation, mode, None).await
}

pub(super) type FragmentRenderBarrier = Box<dyn FnOnce() + Send>;

pub(super) async fn fragment_response(
    state: HttpState,
    region_id: String,
    requested_generation: Option<u64>,
    mode: RenderMode,
    barrier: Option<FragmentRenderBarrier>,
) -> Response {
    // Moving the state handle and request scalars into the blocking pool is the
    // reactor's entire contribution. Lock acquisition, the potentially deep
    // ReadingRegion clone, and rendering all happen off-thread. The guard is
    // checked against the same locked snapshot that supplies the clone.
    match tokio::task::spawn_blocking(move || {
        let region = {
            let review = state.review.read().map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "review projection unavailable",
                )
            })?;
            lookup_fragment(&review, &region_id, requested_generation)?.clone()
        };
        if let Some(barrier) = barrier {
            barrier();
        }
        Ok::<_, (StatusCode, &'static str)>(render_region(&region, true, mode))
    })
    .await
    {
        Ok(Ok(html)) => Html(html).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "fragment render worker unavailable",
        )
            .into_response(),
    }
}

pub(super) fn lookup_fragment<'a>(
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

pub(super) fn json_bad(rejection: axum::extract::rejection::JsonRejection) -> Response {
    (
        StatusCode::BAD_REQUEST,
        format!("invalid action JSON: {rejection}"),
    )
        .into_response()
}

pub(super) async fn extra_stylesheet(State(state): State<HttpState>) -> Response {
    match state.extra_css.as_deref() {
        Some(css) => (
            [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
            css.to_string(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

pub(super) async fn interaction(
    State(state): State<HttpState>,
    Json(report): Json<InteractionReport>,
) -> Response {
    let result = state
        .interactions
        .lock()
        .map_err(|_| "interaction registry unavailable")
        .and_then(|mut interactions| interactions.report(report));
    match result {
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
        Ok(true) => {
            // Only publish the latest accepted input time. The blocking
            // coordinator applies registry throttling and performs disk I/O.
            state.heartbeat.record(Utc::now());
        }
        Ok(false) => {}
    }
    StatusCode::NO_CONTENT.into_response()
}

pub(super) fn render_shell(state: &HttpState) -> String {
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
    out.push_str("</script><style nonce=\"");
    escape_to(&mut out, &state.style_nonce);
    out.push_str("\">");
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

pub(super) fn render_overview(review: &WebReview) -> String {
    web_render::render_overview(
        review,
        &review.target,
        RenderOptions::live(RenderMode::Guided, false),
    )
}

pub(super) fn render_overview_with_target(review: &WebReview, target: &str) -> String {
    web_render::render_overview(
        review,
        target,
        RenderOptions::live(RenderMode::Guided, false),
    )
}

pub(super) fn render_coverage(review: &WebReview) -> String {
    web_render::render_coverage(review)
}

pub(super) fn render_footer(review: &WebReview) -> String {
    web_render::render_footer(review)
}

pub(super) fn render_file_tree_html(review: &WebReview) -> String {
    web_render::render_file_tree(review, RenderOptions::live(RenderMode::Guided, false))
}

pub(super) fn render_region(region: &ReadingRegion, fragment: bool, mode: RenderMode) -> String {
    web_render::render_region(region, mode, RenderOptions::live(mode, fragment))
}

pub(super) fn render_region_skeleton(region: &ReadingRegion) -> String {
    let mut out = String::from("<section id=\"");
    escape_to(&mut out, &region.id);
    out.push_str("\" class=\"region region-skeleton\" data-region=\"");
    escape_to(&mut out, &region.id);
    out.push_str("\"><div class=\"skeleton-label\"><strong>");
    escape_to(&mut out, &web_render::region_label(region));
    out.push_str(
        "</strong></div><div class=\"skeleton-lines\" aria-hidden=\"true\"></div></section>",
    );
    out
}

pub(super) fn query_value<'a>(query: Option<&'a str>, key: &str) -> Option<&'a str> {
    query?.split('&').find_map(|pair| {
        let (candidate, value) = pair.split_once('=')?;
        (candidate == key).then_some(value)
    })
}

pub(super) fn escape_to(out: &mut String, value: &str) {
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
