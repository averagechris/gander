use super::*;

pub(super) struct WebWatcher {
    pub(super) state_path: PathBuf,
    pub(super) overlay_path: PathBuf,
    pub(super) state_mtime: Option<SystemTime>,
    pub(super) overlay_mtime: Option<SystemTime>,
    pub(super) repo_fingerprint: Option<String>,
    pub(super) last_repo_poll: Option<std::time::Instant>,
    pub(super) jj: Box<dyn JjBackend + Send>,
    pub(super) ignore_globs: Vec<String>,
    pub(super) generated_matcher: GeneratedMatcher,
}

impl WebWatcher {
    pub(super) fn new(params: &mut WebParams) -> Self {
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

    pub(super) fn poll_files(
        &mut self,
        session: &mut ReviewSession,
        live_state: &mut review::LiveStateHandle,
    ) {
        let state_mtime = file_mtime(&self.state_path);
        if state_mtime.is_some() && state_mtime != self.state_mtime {
            live_state.replace_current(session.to_state());
            match live_state.reload().cloned() {
                Ok(merged) => {
                    session.apply_review_state(merged);
                    self.state_mtime = state_mtime;
                }
                Err(error) => eprintln!("gander web: review-state reload failed: {error}"),
            }
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

    pub(super) fn poll_repo(&mut self, session: &mut ReviewSession) {
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
        if let Err(error) = self.jj.snapshot_working_copy(&session.repo) {
            eprintln!("gander web: watcher snapshot failed: {error}");
            return;
        }
        let fingerprint = match self.jj.change_fingerprint(&session.repo, &session.target) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                eprintln!("gander web: watcher fingerprint failed: {error}");
                return;
            }
        };
        let changed = self
            .repo_fingerprint
            .as_ref()
            .is_some_and(|previous| previous != &fingerprint);
        self.repo_fingerprint = Some(fingerprint);
        if changed && let Err(error) = self.reload_target(session) {
            eprintln!("gander web: watcher target reload failed: {error}");
        }
    }

    pub(super) fn reload_local_state(
        &mut self,
        session: &mut ReviewSession,
        live_state: &mut review::LiveStateHandle,
    ) -> Result<()> {
        live_state.replace_current(session.to_state());
        session.apply_review_state(live_state.reload()?.clone());
        self.state_mtime = file_mtime(&self.state_path);
        let overlay = crate::agent::AgentOverlay::load_or_default(&self.overlay_path)?;
        session.apply_agent_overlay(&overlay);
        session.touch_stream_inputs();
        self.overlay_mtime = file_mtime(&self.overlay_path);
        Ok(())
    }

    pub(super) fn reload_target(&self, session: &mut ReviewSession) -> Result<()> {
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

pub(super) fn file_mtime(path: &std::path::Path) -> Option<SystemTime> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

pub(super) fn reload_chapter_metadata(jj: &dyn JjBackend, session: &mut ReviewSession) {
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

pub(crate) fn run(params: WebParams) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .wrap_err("failed to start the web runtime")?;
    runtime.block_on(run_async(params))
}

pub(super) async fn run_async(mut params: WebParams) -> Result<()> {
    let listener = TcpListener::bind(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        params.port,
    ))
    .await
    .with_context(|| format!("failed to bind 127.0.0.1:{}", params.port))?;
    let address = listener.local_addr()?;
    debug_assert_eq!(address.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    // Building the shared projection can parse and materialize a large diff.
    // Do it before serving, but never on the current-thread Tokio reactor.
    let (returned, initial_review, initial_rendered, initial_recovery) =
        tokio::task::spawn_blocking(move || {
            let review = WebReview::from_session(&params.session);
            let rendered = rendered_projection(&review);
            let recovery = recovery_projection_event(&rendered, review.generation);
            (params, review, rendered, recovery)
        })
        .await
        .wrap_err("web projection bootstrap worker stopped")?;
    params = returned;
    let worker_shutdown = Arc::new(AtomicBool::new(false));
    let process_control = JjProcessControl::new(worker_shutdown.clone(), WATCH_JJ_TIMEOUT);
    params
        .acp_jj
        .configure_process_control(process_control.clone());
    params
        .watch_jj
        .as_mut()
        .expect("web watcher backend is present")
        .configure_process_control(process_control);
    let watcher = WebWatcher::new(&mut params);

    let bridge = AcpBridge::bind(
        params.socket_path.clone(),
        params.overlay_path.clone(),
        Some(params.acp_jj),
    )?;
    let now = Utc::now();
    let registration = InstanceRegistration::register(
        &params.registry_dir,
        InstanceInfo {
            pid: std::process::id(),
            workspace_root: params.workspace_root.clone(),
            base: params.session.target.base.clone(),
            rev: params.session.target.rev.clone(),
            summary: params.session.summary_line(),
            socket_path: params.socket_path.clone(),
            started_at: now,
            last_input_at: now,
        },
    )?;
    // This guard is intentionally not shared with the blocking worker. Even if
    // arbitrary synchronous Rust code cannot be cancelled, clean shutdown can
    // stop advertising and unlink its socket without waiting for that thread.
    let endpoint_cleanup = EndpointCleanup {
        socket_path: params.socket_path.clone(),
        registration_path: params
            .registry_dir
            .join(format!("{}.json", std::process::id())),
    };

    let review = Arc::new(RwLock::new(initial_review));
    let (events, _) = tokio::sync::broadcast::channel(SSE_BROADCAST_CAPACITY);
    // A watch channel deliberately retains only the newest presenter move.
    // Fast agent driving therefore cannot queue a browser scroll storm.
    let (present_tx, present_rx) = tokio::sync::watch::channel(None);
    let (action_tx, action_rx) = std::sync::mpsc::sync_channel(16);
    let action_rx = Arc::new(Mutex::new(action_rx));
    let (stream_shutdown_tx, stream_shutdown_rx) = tokio::sync::watch::channel(false);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let worker_failure = Arc::new(Mutex::new(None::<String>));
    let token = uuid::Uuid::new_v4().to_string();
    let style_nonce = uuid::Uuid::new_v4().simple().to_string();
    let content_security_policy = live_content_security_policy(&style_nonce);
    let host = format!("127.0.0.1:{}", address.port());
    let origin = format!("http://{host}");
    let url = format!("{origin}/?token={token}");
    let heartbeat = RegistryHeartbeat::default();
    let http_state = HttpState {
        token: Arc::from(token),
        expected_host: Arc::from(host),
        expected_origin: Arc::from(origin),
        summary: Arc::from(params.session.summary_line()),
        base: Arc::from(params.session.target.base.clone()),
        rev: Arc::from(params.session.target.rev.clone()),
        target: Arc::from(params.session.target.to_string()),
        theme_css: Arc::from(render_theme_css(&params.theme)),
        style_nonce: Arc::from(style_nonce),
        content_security_policy: Arc::from(content_security_policy),
        worker_healthy: Arc::new(AtomicBool::new(true)),
        review,
        recovery: Arc::new(RwLock::new(initial_recovery)),
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
        heartbeat: heartbeat.clone(),
        actions: action_tx,
    };
    let app = router(http_state.clone());

    println!("{url}");
    if params.no_open {
        eprintln!("not opening browser (--no-open); use the printed URL");
    } else if let Err(error) = open_url(&url) {
        eprintln!("warning: failed to open browser ({error}); use the printed URL");
    }

    let signal_worker_shutdown = worker_shutdown.clone();
    let signal_actions = action_rx.clone();
    let signal_stream_shutdown = stream_shutdown_tx.clone();
    let signal_http_shutdown = shutdown_tx.clone();
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        signal_worker_shutdown.store(true, Ordering::Release);
        fail_queued_actions(&signal_actions, "review action service is shutting down");
        let _ = signal_stream_shutdown.send(true);
        let _ = signal_http_shutdown.send(true);
    });
    let worker_http = http_state.clone();
    let worker_flag = worker_shutdown.clone();
    let worker_session = params.session;
    let worker_state_path = params.state_path;
    let worker = spawn_blocking_worker(
        BlockingWorker {
            session: worker_session,
            state_path: worker_state_path,
            watcher,
            bridge,
            registration,
            heartbeat,
            rendered: initial_rendered,
            actions: action_rx.clone(),
            present_tx,
            http: worker_http,
            shutdown: worker_flag,
        },
        WorkerFailure {
            healthy: http_state.worker_healthy.clone(),
            shutdown: worker_shutdown.clone(),
            actions: action_rx.clone(),
            stream_shutdown: stream_shutdown_tx,
            http_shutdown: shutdown_tx,
            error: worker_failure.clone(),
        },
    );
    let connections = ActiveHttpConnections::default();
    let server_result = serve_http_until_shutdown(
        listener,
        app,
        shutdown_rx,
        connections,
        HTTP_GRACEFUL_SHUTDOWN_TIMEOUT,
    )
    .await;
    worker_shutdown.store(true, Ordering::Release);
    fail_queued_actions(&action_rx, "review action service stopped");
    // Endpoint ownership is independent of the worker: unlink before the
    // bounded join so a stuck filesystem/render fake cannot remain discoverable.
    drop(endpoint_cleanup);
    worker.shutdown();
    drop(http_state);
    server_result.wrap_err("web server failed")?;
    if let Some(error) = worker_failure
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        return Err(color_eyre::eyre::eyre!(error));
    }
    Ok(())
}

pub(super) fn open_url(url: &str) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";

    open_url_with(program, url)
}

pub(super) fn open_url_with(program: &str, url: &str) -> io::Result<()> {
    let mut child = Command::new(program)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[derive(Clone, Default)]
pub(super) struct ActiveHttpConnections {
    pub(super) inner: Arc<Mutex<ActiveHttpConnectionsInner>>,
}

#[derive(Default)]
pub(super) struct ActiveHttpConnectionsInner {
    pub(super) next_id: u64,
    pub(super) sockets: BTreeMap<u64, std::net::TcpStream>,
}

impl ActiveHttpConnections {
    pub(super) fn track(&self, stream: TcpStream) -> io::Result<Option<TrackedTcpStream>> {
        {
            let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            if inner.sockets.len() >= MAX_HTTP_CONNECTIONS {
                // Dropping the just-accepted stream immediately refuses excess
                // clients, including clients that never send an auth token.
                return Ok(None);
            }
        }
        let stream = stream.into_std()?;
        let control = stream.try_clone()?;
        let stream = TcpStream::from_std(stream)?;
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let id = inner.next_id;
        inner.next_id = inner.next_id.wrapping_add(1);
        inner.sockets.insert(id, control);
        drop(inner);
        Ok(Some(TrackedTcpStream {
            stream,
            id,
            connections: self.clone(),
        }))
    }

    pub(super) fn abort_all(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        for socket in inner.sockets.values() {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        inner.sockets.clear();
    }
}

pub(super) struct TrackedTcpListener {
    pub(super) listener: TcpListener,
    pub(super) connections: ActiveHttpConnections,
}

impl axum::serve::Listener for TrackedTcpListener {
    type Io = TrackedTcpStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.listener.accept().await {
                Ok((stream, address)) => match self.connections.track(stream) {
                    Ok(Some(stream)) => return (stream, address),
                    Ok(None) => continue,
                    Err(error) => eprintln!("gander web: failed to track HTTP connection: {error}"),
                },
                Err(error) => {
                    eprintln!("gander web: failed to accept HTTP connection: {error}");
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

pub(super) struct TrackedTcpStream {
    pub(super) stream: TcpStream,
    pub(super) id: u64,
    pub(super) connections: ActiveHttpConnections,
}

impl Drop for TrackedTcpStream {
    fn drop(&mut self) {
        self.connections
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .sockets
            .remove(&self.id);
    }
}

impl AsyncRead for TrackedTcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(context, buffer)
    }
}

impl AsyncWrite for TrackedTcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(context)
    }
}

pub(super) async fn shutdown_requested(shutdown: &mut tokio::sync::watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

pub(super) async fn serve_http_until_shutdown(
    listener: TcpListener,
    app: Router,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    connections: ActiveHttpConnections,
    graceful_timeout: Duration,
) -> io::Result<()> {
    let tracked_listener = TrackedTcpListener {
        listener,
        connections: connections.clone(),
    };
    let mut graceful_shutdown = shutdown.clone();
    let server = axum::serve(tracked_listener, app)
        .with_graceful_shutdown(async move {
            shutdown_requested(&mut graceful_shutdown).await;
        })
        .into_future();
    tokio::pin!(server);

    tokio::select! {
        result = &mut server => result,
        () = shutdown_requested(&mut shutdown) => {
            match tokio::time::timeout(graceful_timeout, &mut server).await {
                Ok(result) => result,
                Err(_) => {
                    eprintln!(
                        "gander web: HTTP connections did not drain within the shutdown deadline"
                    );
                    // Axum owns detached tasks for accepted connections. Closing
                    // every tracked socket makes those tasks drop their request
                    // futures; leaving this scope then drops the server future.
                    connections.abort_all();
                    Ok(())
                }
            }
        }
    }
}

pub(super) struct EndpointCleanup {
    pub(super) socket_path: PathBuf,
    pub(super) registration_path: PathBuf,
}

impl Drop for EndpointCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
        let _ = fs::remove_file(&self.registration_path);
    }
}

pub(super) struct BlockingWorkerHandle {
    pub(super) join: Option<std::thread::JoinHandle<()>>,
    pub(super) finished: BlockingReceiver<()>,
}

pub(super) struct WorkerFailure {
    pub(super) healthy: Arc<AtomicBool>,
    pub(super) shutdown: Arc<AtomicBool>,
    pub(super) actions: Arc<Mutex<BlockingReceiver<ActionEnvelope>>>,
    pub(super) stream_shutdown: tokio::sync::watch::Sender<bool>,
    pub(super) http_shutdown: tokio::sync::watch::Sender<bool>,
    pub(super) error: Arc<Mutex<Option<String>>>,
}

pub(super) fn spawn_blocking_worker(
    worker: BlockingWorker,
    failure: WorkerFailure,
) -> BlockingWorkerHandle {
    spawn_blocking_task(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_blocking_worker(worker);
        }));
        if let Err(payload) = outcome {
            let detail = if let Some(message) = payload.downcast_ref::<&str>() {
                (*message).to_owned()
            } else if let Some(message) = payload.downcast_ref::<String>() {
                message.clone()
            } else {
                "unknown panic payload".to_owned()
            };
            failure.healthy.store(false, Ordering::Release);
            failure.shutdown.store(true, Ordering::Release);
            fail_queued_actions(&failure.actions, "web coordinator panicked");
            *failure
                .error
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some(format!("web coordinator panicked: {detail}"));
            let _ = failure.stream_shutdown.send(true);
            let _ = failure.http_shutdown.send(true);
            eprintln!("gander web: coordinator panicked: {detail}");
        }
    })
}

pub(super) fn spawn_blocking_task(task: impl FnOnce() + Send + 'static) -> BlockingWorkerHandle {
    let (finished_tx, finished) = std::sync::mpsc::channel();
    let join = std::thread::Builder::new()
        .name("gander-web-worker".into())
        .spawn(move || {
            task();
            let _ = finished_tx.send(());
        })
        .expect("failed to start web blocking worker");
    BlockingWorkerHandle {
        join: Some(join),
        finished,
    }
}

impl BlockingWorkerHandle {
    pub(super) fn shutdown(mut self) {
        if self.finished.recv_timeout(WORKER_JOIN_TIMEOUT).is_ok() {
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        } else {
            // Rust cannot cancel arbitrary synchronous compute. Production jj
            // children are separately killed/reaped; detaching here is the
            // final bound for a wedged filesystem/parser or adversarial fake.
            eprintln!("gander web: blocking worker did not stop within the shutdown deadline");
        }
    }
}

/// Owns all mutable review/session state and every potentially blocking jj,
/// filesystem, parse, projection, and render operation. A single worker plus a
/// bounded action channel means polls cannot overlap or build a stale backlog;
/// missed ticks coalesce to one run after the current operation completes.
pub(super) struct BlockingWorker {
    pub(super) session: ReviewSession,
    pub(super) state_path: PathBuf,
    pub(super) watcher: WebWatcher,
    pub(super) bridge: AcpBridge,
    pub(super) registration: InstanceRegistration,
    pub(super) heartbeat: RegistryHeartbeat,
    pub(super) rendered: RenderedProjection,
    pub(super) actions: Arc<Mutex<BlockingReceiver<ActionEnvelope>>>,
    pub(super) present_tx: tokio::sync::watch::Sender<Option<Arc<PresentEvent>>>,
    pub(super) http: HttpState,
    pub(super) shutdown: Arc<AtomicBool>,
}

pub(super) fn run_blocking_worker(worker: BlockingWorker) {
    let BlockingWorker {
        mut session,
        state_path,
        mut watcher,
        mut bridge,
        mut registration,
        heartbeat,
        mut rendered,
        actions,
        present_tx,
        http,
        shutdown,
    } = worker;
    let mut live_state = review::LiveStateHandle::new(state_path.clone(), session.to_state());
    let mut presentation = None;
    let mut present_sequence = 0u64;
    let mut cadence = WorkerCadence::new(std::time::Instant::now());
    while !shutdown.load(Ordering::Acquire) {
        let wait = cadence.wait(std::time::Instant::now());
        let received = actions
            .lock()
            .map(|actions| actions.recv_timeout(wait))
            .unwrap_or(Err(RecvTimeoutError::Disconnected));
        match received {
            Ok(action) => {
                if shutdown.load(Ordering::Acquire) {
                    fail_queued_action(action, "review action service is shutting down");
                    continue;
                }
                // This CAS is the linearization point: cancellation before it
                // guarantees no mutation; cancellation after it cannot turn a
                // running action into a reported failure.
                if !action.lifecycle.claim() {
                    continue;
                }
                let result = process_action(
                    action.expected_generation,
                    action.command,
                    &mut session,
                    &state_path,
                    &mut live_state,
                    &mut watcher,
                    ProjectionPublisher {
                        http: &http,
                        rendered: &mut rendered,
                    },
                );
                action.lifecycle.complete();
                let _ = action.response.send(result);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        flush_registry_heartbeat(
            &heartbeat,
            &mut registration,
            &http.base,
            &http.rev,
            &http.summary,
        );
        if cadence.take_due(std::time::Instant::now()) {
            let before = session.stream_inputs_generation();
            process_acp_requests(
                &mut bridge,
                &mut session,
                &mut live_state,
                &mut watcher,
                &mut presentation,
                &mut present_sequence,
                &present_tx,
                &http,
            );
            watcher.poll_files(&mut session, &mut live_state);
            watcher.poll_repo(&mut session);
            if reconcile_web_presentation(&session, &mut presentation) {
                publish_present_event(
                    &session,
                    &mut present_sequence,
                    &present_tx,
                    "sync",
                    &presentation,
                    None,
                    None,
                );
            }
            if session.stream_inputs_generation() != before {
                publish_projection(&http, &session, &mut rendered);
            }
        }
    }
}

pub(super) fn flush_registry_heartbeat(
    pending: &RegistryHeartbeat,
    registration: &mut InstanceRegistration,
    base: &str,
    rev: &str,
    summary: &str,
) {
    let Some(at) = pending.take() else { return };
    if let Err(error) = registration.record_input_at(base, rev, summary, at) {
        eprintln!("gander web: registry heartbeat failed: {error}");
    }
}

pub(super) fn fail_queued_actions(
    actions: &Arc<Mutex<BlockingReceiver<ActionEnvelope>>>,
    message: &'static str,
) {
    let Ok(actions) = actions.lock() else { return };
    while let Ok(action) = actions.try_recv() {
        fail_queued_action(action, message);
    }
}

pub(super) fn fail_queued_action(action: ActionEnvelope, message: &'static str) {
    if action.lifecycle.cancel_queued() {
        let _ = action.response.send(Err(ActionError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.to_owned(),
            rollback_safe: true,
        }));
    }
}

pub(super) struct WorkerCadence {
    pub(super) next_tick: std::time::Instant,
}

impl WorkerCadence {
    pub(super) fn new(now: std::time::Instant) -> Self {
        Self { next_tick: now }
    }

    pub(super) fn wait(&self, now: std::time::Instant) -> Duration {
        self.next_tick.saturating_duration_since(now)
    }

    /// Claims at most one poll after any number of missed intervals. Scheduling
    /// from completion, rather than replaying interval ticks, prevents overlap
    /// and backlog after a slow jj/render operation.
    pub(super) fn take_due(&mut self, now: std::time::Instant) -> bool {
        if now < self.next_tick {
            return false;
        }
        self.next_tick = now + WATCH_TICK;
        true
    }
}

pub(super) async fn wait_for_shutdown_signal() {
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
