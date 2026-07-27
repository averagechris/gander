use super::*;

pub(super) fn diff_projection(
    previous: &RenderedProjection,
    next: &RenderedProjection,
    generation: u64,
) -> ProjectionEvent {
    let mut patches = Vec::new();
    for (id, (guided, full)) in &next.regions {
        if previous.regions.get(id) != Some(&(guided.clone(), full.clone())) {
            patches.push(RegionPatch {
                id: id.clone(),
                html: None,
                guided: Some(guided.clone()),
                full: Some(full.clone()),
                remove: false,
            });
        }
    }
    for id in previous.regions.keys() {
        if !next.regions.contains_key(id) {
            patches.push(RegionPatch {
                id: id.clone(),
                html: None,
                guided: None,
                full: None,
                remove: true,
            });
        }
    }
    ProjectionEvent {
        generation,
        full: false,
        patches,
        order: next.order.clone(),
    }
}

pub(super) fn rendered_projection(review: &WebReview) -> RenderedProjection {
    #[cfg(test)]
    RENDERED_PROJECTION_PASSES.with(|passes| passes.set(passes.get() + 1));
    let mut regions = std::collections::BTreeMap::new();
    let mut recovery_patches = Vec::new();
    let overview = render_overview(review);
    regions.insert("overview".into(), (overview.clone(), overview.clone()));
    recovery_patches.push(recovery_patch("overview", overview));
    let coverage = render_coverage(review);
    regions.insert("coverage".into(), (coverage.clone(), coverage.clone()));
    recovery_patches.push(recovery_patch("coverage", coverage));
    let file_tree = render_file_tree_html(review);
    regions.insert("file-tree".into(), (file_tree.clone(), file_tree.clone()));
    recovery_patches.push(recovery_patch("file-tree", file_tree));
    let footer = render_footer(review);
    regions.insert("footer".into(), (footer.clone(), footer.clone()));
    recovery_patches.push(recovery_patch("footer", footer));
    for region in &review.projection.regions {
        regions.insert(
            region.id.clone(),
            (
                render_region(region, true, RenderMode::Guided),
                render_region(region, true, RenderMode::Full),
            ),
        );
        recovery_patches.push(recovery_patch(&region.id, render_region_skeleton(region)));
    }
    RenderedProjection {
        regions,
        recovery_patches,
        order: review
            .projection
            .regions
            .iter()
            .map(|region| region.id.clone())
            .collect(),
    }
}

#[cfg(test)]
thread_local! {
    static RENDERED_PROJECTION_PASSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_rendered_projection_passes() {
    RENDERED_PROJECTION_PASSES.with(|passes| passes.set(0));
}

#[cfg(test)]
pub(super) fn rendered_projection_passes() -> usize {
    RENDERED_PROJECTION_PASSES.with(std::cell::Cell::get)
}

pub(super) fn recovery_projection_event(
    rendered: &RenderedProjection,
    generation: u64,
) -> Arc<ProjectionEvent> {
    Arc::new(ProjectionEvent {
        generation,
        full: true,
        patches: rendered.recovery_patches.clone(),
        order: rendered.order.clone(),
    })
}

#[cfg(test)]
pub(super) fn diff_projection_views(previous: &WebReview, next: &WebReview) -> ProjectionEvent {
    diff_projection(
        &rendered_projection(previous),
        &rendered_projection(next),
        next.generation,
    )
}

#[cfg(test)]
pub(super) fn recovery_projection_event_for_view(review: &WebReview) -> Arc<ProjectionEvent> {
    recovery_projection_event(&rendered_projection(review), review.generation)
}

pub(super) fn recovery_patch(id: &str, html: String) -> RegionPatch {
    RegionPatch {
        id: id.to_owned(),
        html: Some(html),
        guided: None,
        full: None,
        remove: false,
    }
}

pub(super) fn sse_state_event(event: &ProjectionEvent) -> SseEvent {
    SseEvent::default()
        .event("state")
        .id(event.generation.to_string())
        .json_data(projection_event_json(event))
        .expect("projection event is JSON serializable")
}

pub(super) fn projection_event_json(event: &ProjectionEvent) -> Value {
    let patches = event
        .patches
        .iter()
        .map(|patch| {
            json!({
                "id": patch.id,
                "html": patch.html,
                "guided": patch.guided,
                "full": patch.full,
                "remove": patch.remove,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "generation": event.generation,
        "full": event.full,
        "patches": patches,
        "order": event.order,
    })
}

pub(super) fn sse_present_event(event: &PresentEvent) -> SseEvent {
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

pub(super) async fn events(State(state): State<HttpState>, request: Request) -> Response {
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
        let current = state_for_stream
            .recovery
            .read()
            .expect("web recovery lock poisoned")
            .clone();
        if client_generation != Some(current.generation)
            && sender
                .send(sse_state_event_blocking(current).await)
                .await
                .is_err()
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
                    if sender
                        .send(sse_state_event_blocking(event).await)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let recovery = state_for_stream
                        .recovery
                        .read()
                        .expect("web recovery lock poisoned")
                        .clone();
                    if sender
                        .send(sse_state_event_blocking(recovery).await)
                        .await
                        .is_err()
                    {
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

pub(super) async fn sse_state_event_blocking(event: Arc<ProjectionEvent>) -> SseEvent {
    tokio::task::spawn_blocking(move || sse_state_event(&event))
        .await
        .unwrap_or_else(|_| {
            SseEvent::default()
                .event("notice")
                .data("state serialization worker unavailable")
        })
}

pub(super) async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "not found")
}
