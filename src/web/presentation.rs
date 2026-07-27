use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn process_acp_requests(
    bridge: &mut AcpBridge,
    session: &mut ReviewSession,
    live_state: &mut review::LiveStateHandle,
    watcher: &mut WebWatcher,
    presentation: &mut Option<WebPresentationState>,
    present_sequence: &mut u64,
    present_tx: &tokio::sync::watch::Sender<Option<Arc<PresentEvent>>>,
    state: &HttpState,
) {
    apply_controlling_focus(session, &state.interactions);
    let (_, _, commands, mutations) = bridge.drain_ui_commands(session);
    for request in mutations {
        let result =
            crate::tui::persist_acp_review_mutation(request.mutation.clone(), session, live_state)
                .map_err(|error| (-32000, error.to_string()));
        request.respond(result);
    }
    for request in commands {
        let command = request.command.clone();
        let result = apply_web_present_command(
            command.clone(),
            session,
            watcher,
            live_state,
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

pub(super) fn apply_web_present_command(
    command: PresentCommand,
    session: &mut ReviewSession,
    watcher: &mut WebWatcher,
    live_state: &mut review::LiveStateHandle,
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
            let target = resolve_present_focus(session, &path, line, end_line)
                .map_err(crate::presentation::PresentError::into_rpc)?;
            session.select_stream_row(target.stream_row, true);
            session.focus = Focus::Diff;
            serde_json::to_value(target.status).map_err(|error| (-32000, error.to_string()))
        }
        PresentCommand::Reload => {
            watcher
                .reload_local_state(session, live_state)
                .map_err(|error| (-32000, error.to_string()))?;
            reconcile_web_presentation(session, presentation);
            Ok(web_present_status(session, presentation))
        }
    }
}

pub(crate) fn resolve_present_focus(
    session: &ReviewSession,
    path: &str,
    line: usize,
    end_line: Option<usize>,
) -> Result<crate::presentation::FocusTarget, crate::presentation::PresentError> {
    crate::presentation::resolve_focus_target(session, path, line, end_line)
}

pub(super) fn goto_web_spotlight(
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

pub(super) fn reconcile_web_presentation(
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

pub(super) fn web_present_status(
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

pub(super) fn publish_present_event(
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

pub(super) fn presentation_target(
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

pub(super) fn resolve_present_target(
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

pub(super) fn apply_controlling_focus(
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
