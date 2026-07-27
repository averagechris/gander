use super::*;

pub(super) fn publish_projection(
    state: &HttpState,
    session: &ReviewSession,
    current_rendered: &mut RenderedProjection,
) {
    // Projection and rendering happen before the short write-lock section.
    // HTTP handlers therefore never wait on jj reads or stream materialization.
    let mut next = WebReview::from_session(session);
    let current = match state.review.read() {
        Ok(current) => current.clone(),
        Err(_) => return,
    };
    next.generation = current.generation;
    // Materialize every guided/full payload once. The retained prior snapshot
    // makes change detection a string comparison rather than a second render.
    let mut next_rendered = rendered_projection(&next);
    if current_rendered.effective_content_eq(&next_rendered) {
        return;
    }
    next.generation = current.generation.saturating_add(1);
    // Generation only affects the small footer, so refresh it without
    // re-rendering the projection after assigning the monotonic generation.
    next_rendered.refresh_footer(&next);
    let event = diff_projection(current_rendered, &next_rendered, next.generation);
    let recovery = recovery_projection_event(&next_rendered, next.generation);
    let Ok(mut stored_review) = state.review.write() else {
        return;
    };
    let Ok(mut stored_recovery) = state.recovery.write() else {
        return;
    };
    // Commit the fragment source and reconnect skeleton as one short critical
    // section. No rendering or await occurs while either lock is held.
    *stored_review = next;
    *stored_recovery = recovery;
    *current_rendered = next_rendered;
    drop(stored_recovery);
    drop(stored_review);
    let _ = state.events.send(Arc::new(event));
}

pub(super) fn session_files(session: &ReviewSession) -> Vec<FileDiff> {
    session.files.iter().map(|file| file.diff.clone()).collect()
}

pub(super) fn active_state_session_index(
    state: &mut ReviewState,
    session: &ReviewSession,
) -> usize {
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

pub(super) fn process_action(
    expected_generation: u64,
    command: ActionCommand,
    session: &mut ReviewSession,
    state_path: &std::path::Path,
    live_state: &mut review::LiveStateHandle,
    watcher: &mut WebWatcher,
    projection: ProjectionPublisher<'_>,
) -> std::result::Result<ActionResult, ActionError> {
    let current = projection
        .http
        .review
        .read()
        .map_err(|_| ActionError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "review projection unavailable".into(),
            rollback_safe: true,
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
                WalkthroughVerb::Start => session.jump_to_spotlight_index(0),
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
            let action = match command {
                ActionCommand::FileViewed { path, viewed } => {
                    review::ReviewAction::FileViewed { path, viewed }
                }
                ActionCommand::Acknowledge { selection } => {
                    review::ReviewAction::Acknowledge(selection)
                }
                ActionCommand::CommentAdd(action) => {
                    review::ReviewAction::CommentAdd(review::AddCommentRequest {
                        path: action.path,
                        line: action.line,
                        end_line: action.end_line,
                        body: action.body,
                        kind: action.kind,
                        action: action.action,
                        state: action.state,
                        channel: action.channel,
                        source_comment_id: action.source_comment_id,
                        anchor: None,
                    })
                }
                ActionCommand::CommentEdit(action) => review::ReviewAction::CommentEdit {
                    id: action.id,
                    edits: review::CommentEdits {
                        body: action.body,
                        kind: action.kind,
                        action: action.action,
                        channel: action.channel,
                        ..Default::default()
                    },
                },
                ActionCommand::CommentReply(action) => review::ReviewAction::CommentReply {
                    id: action.id,
                    body: action.body,
                    resolve: action.resolve,
                },
                ActionCommand::CommentState { id, state } => {
                    review::ReviewAction::CommentState { id, state }
                }
                ActionCommand::DraftAccept { id, body, channel } => {
                    review::ReviewAction::DraftAccept { id, body, channel }
                }
                ActionCommand::DraftDiscard { id } => review::ReviewAction::DraftDiscard { id },
                ActionCommand::Salience {
                    verb,
                    target,
                    salience,
                    rationale,
                } => match verb {
                    SalienceVerb::Set => review::ReviewAction::SalienceSet {
                        path: target.path,
                        line: target.line,
                        end_line: target.end_line,
                        salience: salience
                            .ok_or_else(|| ActionError::bad("salience-set requires salience"))?,
                        rationale,
                    },
                    SalienceVerb::Clear => review::ReviewAction::SalienceClear {
                        path: target.path,
                        line: target.line,
                        end_line: target.end_line,
                    },
                    SalienceVerb::Promote => review::ReviewAction::SaliencePromote {
                        path: target.path,
                        line: target.line,
                        end_line: target.end_line,
                        rationale,
                    },
                    SalienceVerb::Demote => review::ReviewAction::SalienceDemote {
                        path: target.path,
                        line: target.line,
                        end_line: target.end_line,
                        rationale,
                    },
                },
                ActionCommand::Walkthrough { .. } => unreachable!(),
            };
            let outcome = review::apply_review_action(
                &mut state,
                review::ReviewActionContext {
                    session_index,
                    files: &files,
                    author: session.human_identity.clone(),
                    initial_comment_state: session.comment_initial_state,
                    channel_policy: review::CommentChannelPolicy::Contextual {
                        agent_attached: false,
                        configured_human_name: session.configured_human_name.as_deref(),
                        configured_human_email: session.configured_human_email.as_deref(),
                        target_author_name: session.target_author_name.as_deref(),
                        target_author_email: session.target_author_email.as_deref(),
                        fixed_default: session.comment_default_channel,
                    },
                },
                action,
            )
            .map_err(ActionError::bad)?;
            result = serde_json::to_value(outcome).expect("review action outcome serializes");
            session.apply_review_state(state);
        }
    }

    live_state.replace_current(session.to_state());
    let merged = live_state.save().cloned().map_err(|error| ActionError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("failed to save review action: {error}"),
        rollback_safe: false,
    })?;
    session.apply_review_state(merged.clone());
    watcher.state_mtime = file_mtime(state_path);
    publish_projection(projection.http, session, projection.rendered);
    let generation = projection
        .http
        .review
        .read()
        .map_err(|_| ActionError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "review projection unavailable".into(),
            rollback_safe: false,
        })?
        .generation;
    Ok(ActionResult { generation, result })
}

pub(super) struct ProjectionPublisher<'a> {
    pub(super) http: &'a HttpState,
    pub(super) rendered: &'a mut RenderedProjection,
}

pub(super) async fn submit_action(
    state: HttpState,
    expected_generation: u64,
    command: ActionCommand,
) -> Response {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let lifecycle = Arc::new(ActionLifecycle::queued());
    if let Err(error) = state.actions.try_send(ActionEnvelope {
        expected_generation,
        command,
        response: sender,
        lifecycle: lifecycle.clone(),
    }) {
        let response = match error {
            TrySendError::Full(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "review action service busy; retry",
            )
                .into_response(),
            TrySendError::Disconnected(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "review action service unavailable",
            )
                .into_response(),
        };
        return rollback_safe_response(response);
    }
    match await_action_response(lifecycle, receiver, ACTION_RESPONSE_TIMEOUT).await {
        ActionResponseWait::Cancelled => rollback_safe_response(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "review action service response deadline exceeded before execution",
            )
                .into_response(),
        ),
        ActionResponseWait::Response(Ok(Ok(result))) => Json(result).into_response(),
        ActionResponseWait::Response(Ok(Err(error))) => {
            let rollback_safe = error.rollback_safe;
            let response = (error.status, error.message).into_response();
            if rollback_safe {
                rollback_safe_response(response)
            } else {
                response
            }
        }
        ActionResponseWait::Response(Err(_)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "review action service stopped",
        )
            .into_response(),
    }
}

pub(super) fn rollback_safe_response(mut response: Response) -> Response {
    response.headers_mut().insert(
        "x-gander-action-rollback-safe",
        HeaderValue::from_static("true"),
    );
    response
}

pub(super) enum ActionResponseWait {
    Cancelled,
    Response(
        std::result::Result<
            std::result::Result<ActionResult, ActionError>,
            tokio::sync::oneshot::error::RecvError,
        >,
    ),
}

pub(super) async fn await_action_response(
    lifecycle: Arc<ActionLifecycle>,
    mut receiver: tokio::sync::oneshot::Receiver<std::result::Result<ActionResult, ActionError>>,
    deadline: Duration,
) -> ActionResponseWait {
    match tokio::time::timeout(deadline, &mut receiver).await {
        Err(_) if lifecycle.cancel_queued() => ActionResponseWait::Cancelled,
        // The worker won the claim race (or completed while the timer fired).
        // Dropping this request now would turn an authoritative mutation into
        // an apparent failure, so continue waiting for its bounded operation.
        Err(_) => ActionResponseWait::Response(receiver.await),
        Ok(response) => ActionResponseWait::Response(response),
    }
}

pub(super) async fn file_viewed(
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

pub(super) async fn file_unviewed(
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

pub(super) async fn skim_acknowledge(
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

pub(super) async fn skim_acknowledge_all(
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

pub(super) async fn comment_add(
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
pub(super) async fn comment_edit(
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
pub(super) async fn comment_reply(
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
pub(super) async fn comment_state(
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
pub(super) async fn draft_accept(
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
pub(super) async fn draft_discard(
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
pub(super) async fn salience_action(
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
        pub(super) async fn $name(
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

pub(super) async fn walkthrough_start(
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
            verb: WalkthroughVerb::Start,
            step_id: None,
            part: None,
        },
    )
    .await
}

pub(super) async fn walkthrough_next(
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
pub(super) async fn walkthrough_prev(
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
pub(super) async fn walkthrough_goto(
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
