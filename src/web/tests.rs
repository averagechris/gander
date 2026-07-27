use super::*;
use crate::{
    app::{
        ChapterHeader, Coverage, DiffRow, DiffRowKind, ReadingAnnotation, ReadingAnnotationSource,
        ReadingProjection, ReadingRow, SkimFold,
    },
    diff::DiffLineKind,
    jj::{JjChangeSummary, JjOperationSummary, ReviewTarget, TargetAuthor},
    state::{
        AttentionProgressMember, AttentionProgressTarget, Comment, Walkthrough, WalkthroughStep,
    },
    theme::Rgb,
    web_render::GuideFile,
};
use std::{
    io::{Read, Write},
    net::TcpStream as StdTcpStream,
    path::Path as FsPath,
    sync::atomic::{AtomicUsize, Ordering},
};

#[test]
fn opener_uses_direct_process_and_reports_spawn_failure() {
    let fake_opener = std::env::current_exe().unwrap();
    open_url_with(
        fake_opener.to_str().unwrap(),
        "http://127.0.0.1:1/?token=gander%20web%20';%20rm%20-rf%20nope'",
    )
    .unwrap();

    let missing =
        open_url_with("/definitely/not/a/gander-opener", "http://127.0.0.1:1/").unwrap_err();
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
}

#[test]
fn runtime_always_prints_url_and_no_open_suppresses_only_invocation() {
    let url = "http://127.0.0.1:8123/?token=secret";
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    announce_url(url, true, &mut stdout, &mut stderr, |_| {
        panic!("--no-open must not invoke the opener")
    })
    .unwrap();
    assert_eq!(String::from_utf8(stdout).unwrap(), format!("{url}\n"));
    assert!(String::from_utf8(stderr).unwrap().contains("--no-open"));

    let calls = AtomicUsize::new(0);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    announce_url(url, false, &mut stdout, &mut stderr, |opened| {
        assert_eq!(opened, url);
        calls.fetch_add(1, Ordering::Relaxed);
        Err(io::Error::new(io::ErrorKind::NotFound, "missing opener"))
    })
    .unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(String::from_utf8(stdout).unwrap(), format!("{url}\n"));
    let warning = String::from_utf8(stderr).unwrap();
    assert!(warning.contains("warning: failed to open browser"));
    assert!(warning.contains("use the printed URL"));
}

async fn test_action(State(state): State<HttpState>) -> Response {
    submit_action(
        state,
        42,
        ActionCommand::FileViewed {
            path: "src/lib.rs".into(),
            viewed: true,
        },
    )
    .await
}

fn action_request(address: SocketAddr) -> std::thread::JoinHandle<io::Result<String>> {
    std::thread::spawn(move || {
        let mut stream = StdTcpStream::connect(address)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        write!(
            stream,
            "POST /action HTTP/1.1\r\nHost: {address}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        Ok(response)
    })
}

fn raw_request(address: SocketAddr, request: String) -> io::Result<String> {
    let mut stream = StdTcpStream::connect(address)?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.write_all(request.as_bytes())?;
    let mut response = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                response.extend_from_slice(&buffer[..read]);
                if response.windows(4).any(|window| window == b"\r\n\r\n")
                    && request.starts_with("GET /events")
                {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(String::from_utf8_lossy(&response).into_owned())
}

fn request_text(
    method: &str,
    path: &str,
    host: Option<&str>,
    origin: Option<&str>,
    body: &str,
) -> String {
    let mut request = format!("{method} {path} HTTP/1.1\r\n");
    if let Some(host) = host {
        request.push_str(&format!("Host: {host}\r\n"));
    }
    if let Some(origin) = origin {
        request.push_str(&format!("Origin: {origin}\r\n"));
    }
    request.push_str(&format!(
        "Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    request
}

fn response_status(response: &str) -> u16 {
    response
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse().ok())
        .unwrap_or(0)
}

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
                target: AttentionProgressTarget {
                    members: vec![AttentionProgressMember {
                        file: "generated.lock".into(),
                        line: Some(1),
                        end_line: Some(20),
                    }],
                },
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
    let (actions, _receiver) = std::sync::mpsc::sync_channel(1);
    let recovery = recovery_projection_event_for_view(&review);
    let style_nonce = "test-session-nonce";
    HttpState {
        token: Arc::from("safe-token"),
        expected_host: Arc::from("127.0.0.1:8123"),
        expected_origin: Arc::from("http://127.0.0.1:8123"),
        summary: Arc::from("summary"),
        base: Arc::from("main"),
        rev: Arc::from("@"),
        target: Arc::from("main..@"),
        theme_css: Arc::from(render_theme_css(&ThemeConfig::default())),
        style_nonce: Arc::from(style_nonce),
        content_security_policy: Arc::from(live_content_security_policy(style_nonce)),
        worker_healthy: Arc::new(AtomicBool::new(true)),
        review: Arc::new(RwLock::new(review)),
        recovery: Arc::new(RwLock::new(recovery)),
        events: tokio::sync::broadcast::channel(SSE_BROADCAST_CAPACITY).0,
        present_events: tokio::sync::watch::channel(None).1,
        interactions: Arc::new(Mutex::new(WebInteractions::default())),
        shutdown: tokio::sync::watch::channel(false).1,
        extra_css: None,
        heartbeat: RegistryHeartbeat::default(),
        actions,
    }
}

#[test]
fn production_router_central_guard_covers_every_route_over_loopback() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let address = listener.local_addr().unwrap();
            let host = address.to_string();
            let origin = format!("http://{host}");
            let mut state = http_state(review_fixture());
            state.expected_host = Arc::from(host.clone());
            state.expected_origin = Arc::from(origin.clone());
            let expected_csp = state.content_security_policy.to_string();
            let app = router(state);
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let server = tokio::spawn(serve_http_until_shutdown(
                listener,
                app,
                shutdown_rx,
                ActiveHttpConnections::default(),
                Duration::from_secs(1),
            ));

            let routes = [
                ("GET", "/?token=safe-token", "", 200),
                ("GET", "/assets/app.css?token=safe-token", "", 200),
                ("GET", "/assets/app.js?token=safe-token", "", 200),
                ("GET", "/assets/extra.css?token=safe-token", "", 404),
                ("GET", "/events?token=safe-token&tab=router-test", "", 200),
                (
                    "GET",
                    "/fragment/file-core?token=safe-token&generation=42&mode=guided",
                    "",
                    200,
                ),
                (
                    "POST",
                    "/interaction?token=safe-token",
                    r#"{"tab_id":"absent","client_sequence":1}"#,
                    204,
                ),
                (
                    "POST",
                    "/actions/file-viewed?token=safe-token",
                    r#"{"expected_generation":42,"path":"src/lib.rs"}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/file-unviewed?token=safe-token",
                    r#"{"expected_generation":42,"path":"src/lib.rs"}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/skim-acknowledge?token=safe-token",
                    r#"{"expected_generation":42,"fold_id":"fold-1"}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/skim-acknowledge-all?token=safe-token",
                    r#"{"expected_generation":42}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/comment-add?token=safe-token",
                    r#"{"expected_generation":42,"path":"src/lib.rs","body":"body"}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/comment-edit?token=safe-token",
                    r#"{"expected_generation":42,"id":"comment-1","body":"body"}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/comment-reply?token=safe-token",
                    r#"{"expected_generation":42,"id":"comment-1","body":"reply"}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/comment-state?token=safe-token",
                    r#"{"expected_generation":42,"id":"comment-1","state":"draft"}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/draft-accept?token=safe-token",
                    r#"{"expected_generation":42,"id":"draft-1"}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/draft-discard?token=safe-token",
                    r#"{"expected_generation":42,"id":"draft-1"}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/salience-set?token=safe-token",
                    r#"{"expected_generation":42,"target":{"path":"src/lib.rs"},"salience":"supporting"}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/salience-clear?token=safe-token",
                    r#"{"expected_generation":42,"target":{"path":"src/lib.rs"}}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/salience-promote?token=safe-token",
                    r#"{"expected_generation":42,"target":{"path":"src/lib.rs"}}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/salience-demote?token=safe-token",
                    r#"{"expected_generation":42,"target":{"path":"src/lib.rs"}}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/walkthrough-start?token=safe-token",
                    r#"{"expected_generation":42}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/walkthrough-next?token=safe-token",
                    r#"{"expected_generation":42}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/walkthrough-prev?token=safe-token",
                    r#"{"expected_generation":42}"#,
                    500,
                ),
                (
                    "POST",
                    "/actions/walkthrough-goto?token=safe-token",
                    r#"{"expected_generation":42,"step_id":"step-1"}"#,
                    500,
                ),
                ("GET", "/missing?token=safe-token", "", 404),
            ];
            for (method, valid_path, body, expected) in routes {
                let missing = valid_path.replace("?token=safe-token", "?");
                let wrong = valid_path.replace("token=safe-token", "token=wrong");
                for (path, request_host, request_origin, status, label, guarded) in [
                    (
                        missing.as_str(),
                        Some(host.as_str()),
                        Some(origin.as_str()),
                        401,
                        "missing token",
                        true,
                    ),
                    (
                        wrong.as_str(),
                        Some(host.as_str()),
                        Some(origin.as_str()),
                        401,
                        "wrong token",
                        true,
                    ),
                    (
                        valid_path,
                        None,
                        Some(origin.as_str()),
                        400,
                        "missing host",
                        false,
                    ),
                    (
                        valid_path,
                        Some("attacker.invalid"),
                        Some(origin.as_str()),
                        400,
                        "wrong host",
                        true,
                    ),
                    (
                        valid_path,
                        Some(host.as_str()),
                        Some("http://attacker.invalid"),
                        403,
                        "wrong origin",
                        true,
                    ),
                    (
                        valid_path,
                        Some(host.as_str()),
                        None,
                        expected,
                        "missing origin",
                        true,
                    ),
                    (
                        valid_path,
                        Some(host.as_str()),
                        Some(origin.as_str()),
                        expected,
                        "correct credentials",
                        true,
                    ),
                ] {
                    let request = request_text(method, path, request_host, request_origin, body);
                    let response =
                        tokio::task::spawn_blocking(move || raw_request(address, request))
                            .await
                            .unwrap()
                            .unwrap();
                    assert_eq!(
                        response_status(&response),
                        status,
                        "{method} {valid_path}: {label}: {response:?}"
                    );
                    if guarded {
                        assert!(response.contains("cache-control: no-store"));
                        assert!(
                            response.contains(&format!("content-security-policy: {expected_csp}"))
                        );
                    }
                }
            }
            shutdown_tx.send(true).unwrap();
            server.await.unwrap().unwrap();
        });
}

#[test]
fn live_csp_hashes_exact_emitted_scripts_and_nonces_dynamic_style() {
    let state = http_state(review_fixture());
    let html = render_shell(&state);
    let policy = &state.content_security_policy;
    assert!(!policy.contains("unsafe-inline"));
    assert!(!policy.contains("unsafe-eval"));
    assert!(policy.contains("font-src 'none'"));
    assert!(policy.contains("script-src 'self'"));
    assert!(policy.contains("style-src 'self' 'nonce-test-session-nonce'"));
    assert!(html.contains("<style nonce=\"test-session-nonce\">"));
    for script in [PREPAINT_SCRIPT, THEME_CONTROL_SCRIPT] {
        assert!(html.contains(&format!(">{script}</script>")));
        assert!(policy.contains(&script_hash_source(script)));
        let altered = format!("{script} ");
        assert_ne!(script_hash_source(script), script_hash_source(&altered));
        assert!(!policy.contains(&script_hash_source(&altered)));
    }
}

#[test]
fn accepted_connection_cap_refuses_and_releases_without_timing() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let connections = ActiveHttpConnections::default();
        let mut clients = Vec::new();
        let mut tracked = Vec::new();
        for _ in 0..MAX_HTTP_CONNECTIONS {
            clients.push(TcpStream::connect(address).await.unwrap());
            let (server, _) = listener.accept().await.unwrap();
            tracked.push(connections.track(server).unwrap().expect("within cap"));
        }
        clients.push(TcpStream::connect(address).await.unwrap());
        let (excess, _) = listener.accept().await.unwrap();
        assert!(connections.track(excess).unwrap().is_none());
        drop(tracked.pop());
        clients.push(TcpStream::connect(address).await.unwrap());
        let (replacement, _) = listener.accept().await.unwrap();
        assert!(connections.track(replacement).unwrap().is_some());
    });
}

#[test]
fn sse_connection_holds_and_relinquishes_accepted_slot() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let address = listener.local_addr().unwrap();
            let host = address.to_string();
            let mut state = http_state(review_fixture());
            state.expected_host = Arc::from(host.clone());
            state.expected_origin = Arc::from(format!("http://{host}"));
            let connections = ActiveHttpConnections::default();
            let observed_connections = connections.clone();
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let server = tokio::spawn(serve_http_until_shutdown(
                listener,
                router(state),
                shutdown_rx,
                connections,
                Duration::from_secs(1),
            ));
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let client = std::thread::spawn(move || {
                let mut stream = StdTcpStream::connect(address).unwrap();
                stream
                    .write_all(
                        format!(
                            "GET /events?token=safe-token&tab=permit-test HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 1024];
                while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).unwrap();
                    assert_ne!(read, 0);
                    bytes.extend_from_slice(&buffer[..read]);
                }
                ready_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            tokio::task::spawn_blocking(move || ready_rx.recv().unwrap())
                .await
                .unwrap();
            assert_eq!(
                observed_connections.inner.lock().unwrap().sockets.len(),
                1
            );
            release_tx.send(()).unwrap();
            tokio::task::spawn_blocking(move || client.join().unwrap())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if observed_connections
                        .inner
                        .lock()
                        .unwrap()
                        .sockets
                        .is_empty()
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("SSE socket did not relinquish its connection slot");
            shutdown_tx.send(true).unwrap();
            server.await.unwrap().unwrap();
        });
}

#[test]
fn demo_sized_web_performance_smoke_stays_inside_integrated_proxy_budgets() {
    let review = review_fixture();
    let http = http_state(review.clone());

    // Browser-independent proxy: this measures deterministic server-side
    // rendering, payload size, window/skeleton structure, guarded fragment
    // lookup, surgical SSE patch scope, and presenter latest-value
    // coalescing. It does not claim browser layout, paint, network, or JS
    // execution timing.
    let _warmup = render_shell(&http);
    let started = std::time::Instant::now();
    let html = render_shell(&http);
    let render_elapsed = started.elapsed();
    let meaningful_offset = html.find("Attention map").unwrap_or(usize::MAX);

    assert!(html.len() <= 64 * 1024, "SSR bytes {} > 64KiB", html.len());
    assert!(
        meaningful_offset <= 12 * 1024,
        "initial meaningful marker is too deep in the document"
    );
    assert!(
        render_elapsed <= std::time::Duration::from_millis(250),
        "SSR proxy render took {render_elapsed:?}"
    );
    let skeleton_expected = review
        .projection
        .regions
        .iter()
        .enumerate()
        .filter(|(index, region)| {
            *index >= INITIAL_REGION_WINDOW && !matches!(region.kind, ReadingRegionKind::Chapter(_))
        })
        .count();
    assert_eq!(
        html.matches("data-region=").count(),
        review.projection.regions.len()
    );
    assert_eq!(html.matches("region-skeleton").count(), skeleton_expected);
    assert!(html.contains("data-action=\"file-viewed\""));

    let fragment = lookup_fragment(&review, "file-6", Some(review.generation)).unwrap();
    let fragment_html = render_region(fragment, true, RenderMode::Guided);
    assert!(fragment_html.contains("data-region=\"file-6\""));
    assert_eq!(
        lookup_fragment(&review, "file-6", Some(review.generation - 1))
            .unwrap_err()
            .0,
        StatusCode::CONFLICT
    );

    let mut next = review.clone();
    next.generation += 1;
    next.projection.coverage.covered += 1;
    next.projection.regions.reverse();
    let event = diff_projection_views(&review, &next);
    let patch_bytes: usize = event
        .patches
        .iter()
        .map(|patch| {
            patch.guided.as_ref().map_or(0, String::len)
                + patch.full.as_ref().map_or(0, String::len)
        })
        .sum();
    assert!(!event.full);
    assert_eq!(event.generation, review.generation + 1);
    assert!(
        patch_bytes <= 16 * 1024,
        "patch bytes {patch_bytes} > 16KiB"
    );
    let recovery = recovery_projection_event_for_view(&next);
    let recovery_bytes = serde_json::to_vec(&projection_event_json(&recovery))
        .unwrap()
        .len();
    assert!(recovery.full);
    assert!(
        recovery_bytes <= 8 * 1024,
        "recovery bytes {recovery_bytes} > 8KiB"
    );
    assert!(
        recovery
            .patches
            .iter()
            .all(|patch| patch.guided.is_none() && patch.full.is_none())
    );

    let (present_tx, present_rx) = tokio::sync::watch::channel(None);
    for sequence in 1..=64 {
        present_tx
            .send(Some(Arc::new(PresentEvent {
                sequence,
                command: "focus",
                status: serde_json::json!({"sequence": sequence}),
                target: None,
                note: Some(format!("latest-{sequence}")),
            })))
            .unwrap();
    }
    let latest = present_rx.borrow().as_ref().unwrap().clone();
    assert_eq!(latest.sequence, 64);
    assert_eq!(latest.note.as_deref(), Some("latest-64"));

    eprintln!(
        "web_perf_smoke metrics: ssr_bytes={} meaningful_offset={} render_elapsed={:?} skeleton_regions={} patch_bytes={} recovery_bytes={} latest_presenter_sequence={}",
        html.len(),
        meaningful_offset,
        render_elapsed,
        html.matches("region-skeleton").count(),
        patch_bytes,
        recovery_bytes,
        latest.sequence
    );
}

#[test]
fn effective_projection_change_renders_one_snapshot_pass() {
    let (mut session, _baseline, _dir, _watcher, http) = action_fixture();
    let mut rendered = rendered_projection(&WebReview::from_session(&session));
    let generation = http.review.read().unwrap().generation;
    let mut events = http.events.subscribe();
    session.files[0].viewed = true;
    session.touch_stream_inputs();

    reset_rendered_projection_passes();
    publish_projection(&http, &session, &mut rendered);

    assert_eq!(rendered_projection_passes(), 1);
    assert_eq!(http.review.read().unwrap().generation, generation + 1);
    let event = events.try_recv().expect("effective change publishes once");
    assert_eq!(event.generation, generation + 1);
    assert!(events.try_recv().is_err());
}

#[test]
fn projection_diff_is_monotonic_surgical_and_carries_removals() {
    let previous = review_fixture();
    let mut next = review_fixture();
    next.generation = previous.generation + 1;
    next.projection.coverage.covered += 1;
    next.projection.regions.remove(1);
    let event = diff_projection_views(&previous, &next);
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
    let review = review_fixture();
    let event = recovery_projection_event_for_view(&review);
    assert!(event.full);
    assert_eq!(event.generation, 42);
    assert!(event.patches.iter().any(|patch| patch.id == "overview"));
    assert!(event.patches.iter().any(|patch| patch.id == "footer"));
    assert!(event.patches.iter().any(|patch| patch.id == "file-core"));
    assert_eq!(event.order.first().map(String::as_str), Some("chapter-one"));
    assert!(event.patches.iter().all(|patch| patch.guided.is_none()));
    assert!(event.patches.iter().all(|patch| patch.full.is_none()));
    assert!(event.patches.iter().all(|patch| patch.html.is_some()));
    let payload = serde_json::to_string(
        &event
            .patches
            .iter()
            .map(|patch| (&patch.id, &patch.html, &patch.guided, &patch.full))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(payload.len() < 8 * 1024, "recovery bytes={}", payload.len());
    assert!(!payload.contains("version = 99"));
}

#[test]
fn recovery_and_guided_skim_stay_compact_for_generated_payloads() {
    let mut review = review_fixture();
    let skim = review
        .projection
        .regions
        .iter_mut()
        .find(|region| matches!(region.kind, ReadingRegionKind::Skim(_)))
        .unwrap();
    for index in 0..2_000 {
        skim.rows.push(row(
            &format!("hidden-{index}"),
            "generated.lock",
            &format!("large generated payload {index}"),
            Salience::Skim,
        ));
    }
    let guided = render_region(skim, true, RenderMode::Guided);
    let full = render_region(skim, true, RenderMode::Full);
    let recovery = recovery_projection_event_for_view(&review);
    let recovery_bytes: usize = recovery
        .patches
        .iter()
        .map(|patch| patch.html.as_ref().map_or(0, String::len))
        .sum();
    assert!(guided.len() < 2 * 1024, "guided bytes={}", guided.len());
    assert!(full.len() > 100 * 1024, "full bytes={}", full.len());
    assert!(recovery_bytes < 8 * 1024, "recovery bytes={recovery_bytes}");
    assert!(!guided.contains("large generated payload"));
    assert!(full.contains("large generated payload 1999"));
}

#[test]
fn worker_cadence_coalesces_missed_ticks_without_overlap_or_backlog() {
    let start = std::time::Instant::now();
    let mut cadence = WorkerCadence::new(start);
    assert!(cadence.take_due(start));
    for offset in 1..=100 {
        assert!(!cadence.take_due(start + Duration::from_millis(offset)));
    }
    let after_slow_job = start + WATCH_TICK + Duration::from_secs(5);
    assert!(cadence.take_due(after_slow_job));
    assert!(!cadence.take_due(after_slow_job));
    assert_eq!(cadence.wait(after_slow_job), WATCH_TICK);
}

#[test]
fn fragment_fallback_is_generation_guarded_and_lazy() {
    assert!(COMPONENT_JS.contains("patch.html ||"));
    assert!(COMPONENT_JS.contains("region region-skeleton"));
    assert!(COMPONENT_JS.contains("requestedGeneration !== generation"));
    assert!(COMPONENT_JS.contains("observeLazyRegions"));
    assert!(!COMPONENT_JS.contains("return reload();\n        const replacement"));
}

#[test]
fn slow_fragment_snapshot_and_render_leave_current_thread_reactor_responsive() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let state = http_state(review_fixture());
        let generation = state.review.read().unwrap().generation;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let barrier: FragmentRenderBarrier = Box::new(move || {
            let _ = entered_tx.send(());
            release_rx.lock().unwrap().recv().unwrap();
        });
        let fragment = tokio::spawn(fragment_response(
            state,
            "file-core".into(),
            Some(generation),
            RenderMode::Full,
            Some(barrier),
        ));
        tokio::time::timeout(Duration::from_secs(1), entered_rx)
            .await
            .expect("fragment blocking worker did not reach render barrier")
            .unwrap();

        // A timer heartbeat, lightweight asset handler, and SSE serializer
        // all complete while the deep-cloned fragment is deliberately held.
        tokio::time::timeout(Duration::from_millis(100), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = stylesheet().await.into_response();
            let event = Arc::new(ProjectionEvent {
                generation,
                full: false,
                patches: Vec::new(),
                order: Vec::new(),
            });
            let _ = sse_state_event_blocking(event).await;
        })
        .await
        .expect("fragment work stalled the current-thread reactor");

        release_tx.send(()).unwrap();
        let response = fragment.await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    });
}

#[test]
fn registry_heartbeat_coalesces_and_writes_off_the_reactor() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let start = Utc::now() - chrono::Duration::seconds(3);
        let mut registration = InstanceRegistration::register(
            dir.path(),
            InstanceInfo {
                pid: 91,
                workspace_root: "/repo".into(),
                base: "main".into(),
                rev: "@".into(),
                summary: "summary".into(),
                socket_path: dir.path().join("socket"),
                started_at: start,
                last_input_at: start,
            },
        )
        .unwrap();
        let heartbeat = RegistryHeartbeat::default();
        let latest = start + chrono::Duration::seconds(3);
        for offset in 1..=100 {
            heartbeat.record(start + chrono::Duration::milliseconds(offset * 30));
        }
        assert_eq!(heartbeat.0.lock().unwrap().as_ref(), Some(&latest));

        let worker_heartbeat = heartbeat.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (written_tx, written_rx) = std::sync::mpsc::channel();
        let registry_dir = dir.path().to_path_buf();
        let write = tokio::task::spawn_blocking(move || {
            let _ = entered_tx.send(());
            release_rx.recv().unwrap();
            flush_registry_heartbeat(&worker_heartbeat, &mut registration, "main", "@", "summary");
            written_tx
                .send(crate::registry::list_instances(&registry_dir)[0].last_input_at)
                .unwrap();
        });
        entered_rx.await.unwrap();
        tokio::time::timeout(Duration::from_millis(100), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = stylesheet().await.into_response();
        })
        .await
        .expect("registry write barrier stalled the current-thread reactor");
        release_tx.send(()).unwrap();
        write.await.unwrap();

        assert!(heartbeat.take().is_none(), "burst should flush once");
        assert_eq!(written_rx.recv().unwrap(), latest);
    });
}

#[test]
fn full_mode_fragment_state_machine_discards_stale_guided_replacements() {
    for phrase in [
        "requestedMode !== body.dataset.mode",
        "delete region.dataset.fragmentLoading",
        "return load(region, body.dataset.mode)",
        "body.dataset.mode === \"full\" && replacement.dataset.fullLoaded === \"false\"",
        "return load(replacement, \"full\")",
    ] {
        assert!(
            COMPONENT_JS.contains(phrase),
            "missing full-mode race contract: {phrase}"
        );
    }
    assert!(
        COMPONENT_JS
            .matches("replacement.dataset.fullLoaded === \"false\"")
            .count()
            >= 2,
        "fragment and SSE replacements must both continue full loading"
    );
}

#[test]
fn graceful_http_deadline_aborts_claimed_request_before_endpoint_and_worker_cleanup() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (action_tx, action_rx) = std::sync::mpsc::sync_channel(2);
        let action_rx = Arc::new(Mutex::new(action_rx));
        let mut http = http_state(review_fixture());
        http.actions = action_tx.clone();
        let app = Router::new()
            .route("/action", post(test_action))
            .with_state(http);

        let worker_actions = action_rx.clone();
        let (claimed_tx, claimed_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let stuck_worker = spawn_blocking_task(move || {
            let action = worker_actions.lock().unwrap().recv().unwrap();
            assert!(action.lifecycle.claim());
            let _ = claimed_tx.send(());
            release_rx.recv().unwrap();
            action.lifecycle.complete();
            let _ = action.response.send(Ok(ActionResult {
                generation: 43,
                result: json!({"saved": true}),
            }));
            let _ = finished_tx.send(());
        });

        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("web.sock");
        let registration_path = dir.path().join("instance.json");
        fs::write(&socket_path, "socket").unwrap();
        fs::write(&registration_path, "registration").unwrap();
        let cleanup = EndpointCleanup {
            socket_path: socket_path.clone(),
            registration_path: registration_path.clone(),
        };
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let server = tokio::spawn(async move {
            let result = serve_http_until_shutdown(
                listener,
                app,
                shutdown_rx,
                ActiveHttpConnections::default(),
                Duration::from_millis(50),
            )
            .await;
            drop(cleanup);
            stuck_worker.shutdown();
            result
        });
        let client = action_request(address);
        tokio::time::timeout(Duration::from_secs(2), claimed_rx)
            .await
            .expect("HTTP action was not claimed")
            .unwrap();

        // A second request envelope is still queued when process shutdown
        // begins and therefore must fail rollback-safely without execution.
        let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();
        let queued_lifecycle = Arc::new(ActionLifecycle::queued());
        action_tx
            .send(ActionEnvelope {
                expected_generation: 0,
                command: ActionCommand::FileViewed {
                    path: "src/lib.rs".into(),
                    viewed: true,
                },
                response: response_tx,
                lifecycle: queued_lifecycle.clone(),
            })
            .unwrap();

        let shutdown_started = std::time::Instant::now();
        fail_queued_actions(&action_rx, "review action service stopped");
        shutdown_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(3), server)
            .await
            .expect("server did not honor the graceful HTTP deadline")
            .unwrap()
            .unwrap();
        assert!(shutdown_started.elapsed() < Duration::from_secs(3));
        assert!(!socket_path.exists());
        assert!(!registration_path.exists());
        let error = response_rx.try_recv().unwrap().unwrap_err();
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(queued_lifecycle.0.load(Ordering::Acquire), ACTION_CANCELLED);
        assert!(
            !client
                .join()
                .unwrap()
                .unwrap_or_default()
                .contains("200 OK"),
            "forced connection unexpectedly reported mutation success"
        );

        // The worker remained blocked through server return and endpoint
        // cleanup. Releasing the detached test thread cannot resurrect or
        // retain either endpoint because it never owned their guard.
        release_tx.send(()).unwrap();
        finished_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("detached worker did not finish after its fake was released");
    });
}

#[test]
fn claimed_action_completing_within_http_grace_returns_authoritative_result() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (action_tx, action_rx) = std::sync::mpsc::sync_channel(1);
        let mut http = http_state(review_fixture());
        http.actions = action_tx;
        let app = Router::new()
            .route("/action", post(test_action))
            .with_state(http);
        let (claimed_tx, claimed_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = spawn_blocking_task(move || {
            let action = action_rx.recv().unwrap();
            assert!(action.lifecycle.claim());
            let _ = claimed_tx.send(());
            release_rx.recv().unwrap();
            action.lifecycle.complete();
            action
                .response
                .send(Ok(ActionResult {
                    generation: 43,
                    result: json!({"saved": true}),
                }))
                .unwrap();
        });
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let server = tokio::spawn(async move {
            let result = serve_http_until_shutdown(
                listener,
                app,
                shutdown_rx,
                ActiveHttpConnections::default(),
                Duration::from_secs(1),
            )
            .await;
            worker.shutdown();
            result
        });
        let client = action_request(address);
        tokio::time::timeout(Duration::from_secs(2), claimed_rx)
            .await
            .expect("HTTP action was not claimed")
            .unwrap();

        shutdown_tx.send(true).unwrap();
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("server did not finish its graceful drain")
            .unwrap()
            .unwrap();
        let response = client.join().unwrap().unwrap();
        assert!(response.contains("200 OK"), "response was {response:?}");
        assert!(response.contains("\"generation\":43"));
        assert!(response.contains("\"saved\":true"));
    });
}

#[test]
fn response_deadline_cancels_queued_action_before_any_mutation() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (action_tx, action_rx) = std::sync::mpsc::sync_channel(1);
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let lifecycle = Arc::new(ActionLifecycle::queued());
        action_tx
            .send(ActionEnvelope {
                expected_generation: 0,
                command: ActionCommand::FileViewed {
                    path: "src/lib.rs".into(),
                    viewed: true,
                },
                response: response_tx,
                lifecycle: lifecycle.clone(),
            })
            .unwrap();

        assert!(matches!(
            await_action_response(lifecycle, response_rx, Duration::from_millis(10)).await,
            ActionResponseWait::Cancelled
        ));

        // The worker was blocked past the response deadline. When it can
        // finally dequeue the envelope, the cancelled action cannot be
        // claimed and therefore cannot mutate state.
        let mutations = AtomicUsize::new(0);
        let action = action_rx.recv().unwrap();
        if action.lifecycle.claim() {
            mutations.fetch_add(1, Ordering::SeqCst);
        }
        assert_eq!(mutations.load(Ordering::SeqCst), 0);
        assert_eq!(action.lifecycle.0.load(Ordering::Acquire), ACTION_CANCELLED);
    });
}

#[test]
fn worker_claim_before_deadline_returns_authoritative_generation() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let lifecycle = Arc::new(ActionLifecycle::queued());
        // Deterministically establish the claim-before-deadline ordering.
        // A zero deadline below then forces the timeout branch without a
        // scheduler race.
        assert!(lifecycle.claim());
        let worker_lifecycle = lifecycle.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            worker_lifecycle.complete();
            response_tx
                .send(Ok(ActionResult {
                    generation: 43,
                    result: json!({"saved": true}),
                }))
                .unwrap();
        });

        let response = await_action_response(lifecycle, response_rx, Duration::ZERO).await;
        let ActionResponseWait::Response(Ok(Ok(result))) = response else {
            panic!("claimed action did not return its authoritative success");
        };
        assert_eq!(result.generation, 43);
    });
}

#[test]
fn shutdown_cancels_queued_actions_and_they_cannot_execute() {
    let (action_tx, action_rx) = std::sync::mpsc::sync_channel(1);
    let action_rx = Arc::new(Mutex::new(action_rx));
    let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();
    let lifecycle = Arc::new(ActionLifecycle::queued());
    action_tx
        .send(ActionEnvelope {
            expected_generation: 0,
            command: ActionCommand::FileViewed {
                path: "src/lib.rs".into(),
                viewed: true,
            },
            response: response_tx,
            lifecycle: lifecycle.clone(),
        })
        .unwrap();

    fail_queued_actions(&action_rx, "review action service stopped");

    let error = response_rx.try_recv().unwrap().unwrap_err();
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!lifecycle.claim());
    assert_eq!(lifecycle.0.load(Ordering::Acquire), ACTION_CANCELLED);
    assert!(action_rx.lock().unwrap().try_recv().is_err());
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
        let fingerprint = self.fingerprint.lock().unwrap().clone();
        if fingerprint == "__panic__" {
            panic!("deterministic coordinator panic");
        }
        Ok(fingerprint)
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
fn coordinator_panic_shuts_real_lifecycle_and_cleans_endpoints() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let dir = tempfile::tempdir().unwrap();
        let session = ReviewSession::new(
            dir.path().to_path_buf(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(raw).unwrap(),
            ReviewState::default(),
        );
        let state_path = dir.path().join("state.json");
        session.to_state().save(&state_path).unwrap();
        let socket_path = dir.path().join("web.sock");
        let registry_dir = dir.path().join("registry");
        fs::create_dir(&registry_dir).unwrap();
        let backend = |fingerprint: &str| WatchJj {
            counts: Arc::new(JjCounts::default()),
            fingerprint: Arc::new(Mutex::new(fingerprint.to_owned())),
            diff: raw.into(),
        };
        let result = run_async(WebParams {
            session,
            overlay_path: dir.path().join("agent.json"),
            state_path,
            socket_path: socket_path.clone(),
            registry_dir: registry_dir.clone(),
            workspace_root: dir.path().to_path_buf(),
            acp_jj: Box::new(backend("healthy")),
            watch_jj: Some(Box::new(backend("__panic__"))),
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            port: 0,
            no_open: true,
            theme: ThemeConfig::default(),
            extra_css: None,
        })
        .await;
        let error = result.unwrap_err().to_string();
        assert!(error.contains("web coordinator panicked"), "{error}");
        assert!(error.contains("deterministic coordinator panic"), "{error}");
        assert!(!socket_path.exists());
        assert!(crate::registry::list_instances(&registry_dir).is_empty());
    });
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
    let dir = tempfile::tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let overlay_path = dir.path().join("agent.json");
    let mut external = session.to_state();
    external.comments.push(Comment {
        id: "external".into(),
        path: Some("a.rs".into()),
        line: Some(1),
        body: "from the TUI".into(),
        ..Comment::default()
    });
    external.save(&state_path).unwrap();
    let mut live_state = review::LiveStateHandle::new(state_path.clone(), session.to_state());
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
    watcher.poll_files(&mut session, &mut live_state);
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
    watcher.poll_files(&mut session, &mut live_state);
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
fn central_security_headers_cover_success_errors_sse_and_actions() {
    let responses = [
        ("success", StatusCode::OK.into_response()),
        (
            "error",
            (StatusCode::UNAUTHORIZED, "invalid capability").into_response(),
        ),
        (
            "sse",
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                "event: notice\ndata: ready\n\n",
            )
                .into_response(),
        ),
        ("action", Json(json!({"generation": 2})).into_response()),
    ];
    for (label, response) in responses {
        let response = apply_security_headers(response, "default-src 'none'");
        let headers = response.headers();
        assert_eq!(headers[header::CACHE_CONTROL], "no-store", "{label}");
        assert_eq!(
            headers[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff",
            "{label}"
        );
        assert_eq!(headers["referrer-policy"], "no-referrer", "{label}");
        assert_eq!(headers["x-frame-options"], "DENY", "{label}");
        assert_eq!(
            headers["content-security-policy"], "default-src 'none'",
            "{label}"
        );
    }
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
    review::LiveStateHandle,
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
    let live_state = review::LiveStateHandle::new(watcher.state_path.clone(), baseline);
    (session, live_state, dir, watcher, http)
}

#[test]
fn action_generation_is_preconditioned_and_merge_save_preserves_concurrent_state() {
    let (mut session, mut baseline, _dir, mut watcher, http) = action_fixture();
    let mut rendered = rendered_projection(&WebReview::from_session(&session));
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
        ProjectionPublisher {
            http: &http,
            rendered: &mut rendered,
        },
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
        ProjectionPublisher {
            http: &http,
            rendered: &mut rendered,
        },
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
    let mut baseline = review::LiveStateHandle::new(watcher.state_path.clone(), seeded);
    let http = http_state(WebReview::from_session(&session));
    let mut rendered = rendered_projection(&WebReview::from_session(&session));
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
        ProjectionPublisher {
            http: &http,
            rendered: &mut rendered,
        },
    )
    .unwrap();
    assert_eq!(result.result["source_comment_id"], "onboarding-source");
    assert_eq!(result.result["path"], "a.rs");
    assert_eq!(result.result["line"], 1);
    assert_eq!(result.result["channel"], "delegation");
    assert_eq!(result.result["author"]["kind"], "human");
}

#[test]
fn salience_action_endpoints_return_authoritative_shared_results() {
    let (mut session, _baseline, _dir, mut watcher, _http) = action_fixture();
    let files = session_files(&session);
    let target = crate::attention::target_for_diff(&files, "a.rs", Some(1), None).unwrap();
    let mut seeded = session.to_state();
    let index = active_state_session_index(&mut seeded, &session);
    seeded.sessions[index]
        .attention_regions
        .push(crate::state::AttentionRegion {
            target,
            salience: Salience::Skim,
            rationale: Some("agent fallback".into()),
            source: crate::state::SalienceSource::Agent,
        });
    seeded.save(&watcher.state_path).unwrap();
    session.apply_review_state(seeded.clone());
    let mut baseline = review::LiveStateHandle::new(watcher.state_path.clone(), seeded);
    let http = http_state(WebReview::from_session(&session));
    let mut rendered = rendered_projection(&WebReview::from_session(&session));

    let cases = [
        (
            SalienceVerb::Set,
            Some(Salience::Supporting),
            "supporting",
            "human",
            false,
        ),
        (SalienceVerb::Promote, None, "spotlight", "human", false),
        (SalienceVerb::Demote, None, "supporting", "human", false),
        (SalienceVerb::Clear, None, "skim", "agent", true),
    ];
    for (verb, salience, expected_salience, expected_source, cleared) in cases {
        let generation = http.review.read().unwrap().generation;
        let state_path = watcher.state_path.clone();
        let result = process_action(
            generation,
            ActionCommand::Salience {
                verb,
                target: TargetAction {
                    path: "a.rs".into(),
                    line: Some(1),
                    end_line: None,
                },
                salience,
                rationale: None,
            },
            &mut session,
            &state_path,
            &mut baseline,
            &mut watcher,
            ProjectionPublisher {
                http: &http,
                rendered: &mut rendered,
            },
        )
        .unwrap();
        assert_eq!(
            result.result["target"],
            serde_json::json!({"path":"a.rs","line":1})
        );
        assert_eq!(result.result["effective_salience"], expected_salience);
        assert_eq!(result.result["effective_source"], expected_source);
        assert_eq!(result.result["cleared"], cleared);
        assert!(result.result["target"].get("file").is_none());
        assert!(result.generation > generation);
    }
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
    let salience_handler = COMPONENT_JS
        .split("[\"salience-promote\", \"salience-demote\", \"salience-set\", \"salience-clear\"]")
        .nth(1)
        .expect("salience action handler")
        .split("if (action === \"walkthrough-start\"")
        .next()
        .expect("salience handler boundary");
    assert!(salience_handler.contains("!result?.target || !result?.effective_salience"));
    assert!(salience_handler.contains("targetRows(result.target).forEach"));
    assert!(salience_handler.contains("name === result.effective_salience"));
    assert!(salience_handler.contains("aria-label\", result.effective_salience"));
    assert!(salience_handler.contains("owner?.classList.add(\"action-pending\")"));
    assert!(!salience_handler.contains("result?.target || target"));
    assert!(!salience_handler.contains("action === \"salience-clear\""));
    assert!(!salience_handler.contains("classList.remove(`salience-"));
    assert!(!salience_handler.contains("const order ="));
    assert!(!salience_handler.contains("order.indexOf"));
    let transport_failure = COMPONENT_JS
        .split("const postAction")
        .nth(1)
        .expect("postAction function")
        .split("} catch (error) {")
        .nth(1)
        .and_then(|tail| tail.split("} finally {").next())
        .expect("postAction transport failure branch");
    assert!(!transport_failure.contains("rollback()"));
    assert!(transport_failure.contains("Save outcome unknown; reloading"));
    assert!(transport_failure.contains("setTimeout(reload"));
    assert!(
        COMPONENT_JS
            .contains("response.headers.get(\"x-gander-action-rollback-safe\") === \"true\"")
    );
    assert!(COMPONENT_JS.contains("if (rollbackSafe) rollback()"));
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
        "salience-set",
        "salience-clear",
        "walkthrough-start",
        "walkthrough-next",
        "walkthrough-goto",
        "skim-acknowledge-all",
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
    assert!(html.contains("Start guided tour"));
    assert!(html.contains("data-action=\"walkthrough-start\""));
    assert!(html.contains("aria-label=\"Start guided tour at the first current Spotlight\""));
    assert!(html.contains("id=\"mode-switch\""));
    assert!(html.contains("Full review"));
    assert!(html.contains("id=\"review-search\""));
    assert!(html.contains("class=\"file-tree full-only\""));
    assert!(html.contains("data-action=\"file-viewed\""));
    assert!(html.contains("data-action=\"skim-acknowledge-all\""));
    assert!(html.contains("aria-label=\"Acknowledge all current skim folds\""));
    assert!(html.contains("data-skim-target"));
    for action in [
        "salience-promote",
        "salience-demote",
        "salience-set",
        "salience-clear",
    ] {
        assert!(html.contains(&format!("data-action=\"{action}\"")));
    }
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
fn repo_web_component_style_audit_covers_active_and_dead_sources() {
    assert_eq!(COMPONENT_CSS, include_str!("../web.css"));
    assert_no_literal_colors("component css", COMPONENT_CSS);
    for gradient in ["linear-gradient", "radial-gradient", "conic-gradient"] {
        assert!(!COMPONENT_CSS.contains(gradient));
    }
    assert!(COMPONENT_CSS.contains("var(--foreground)"));

    let legacy_export_source = include_str!("../web_export.rs");
    let dead_constant = ["const ", "CSS", ": &str"].concat();
    let legacy_gradient = ["radial", "-gradient(circle at top left"].concat();
    assert!(!legacy_export_source.contains(&dead_constant));
    assert!(!legacy_export_source.contains(&legacy_gradient));
}

#[test]
fn theme_css_emits_independent_light_and_dark_slots() {
    let css = render_theme_css(&ThemeConfig::default());
    assert!(css.contains("prefers-color-scheme:dark"));
    assert!(css.contains("[data-theme=\"light\"]"));
    assert!(css.contains("[data-theme=\"dark\"]"));
    assert!(!css.contains("data-color-scheme"));
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
        .find("<style nonce=\"test-session-nonce\">")
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
    assert!(PREPAINT_SCRIPT.contains("document.documentElement.dataset.theme=m"));
    assert!(THEME_CONTROL_SCRIPT.contains("['system','light','dark']"));
    assert!(THEME_CONTROL_SCRIPT.contains("delete document.documentElement.dataset.theme"));
    assert!(!THEME_CONTROL_SCRIPT.contains("dataset.colorScheme"));
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
    let baseline = session.to_state();
    baseline.save(&watcher.state_path).unwrap();
    let mut baseline = review::LiveStateHandle::new(watcher.state_path.clone(), baseline);
    crate::agent::AgentOverlay::default()
        .save(&watcher.overlay_path)
        .unwrap();
    let interactions = Arc::new(Mutex::new(WebInteractions::default()));
    let mut presentation = None;
    let apply = |command,
                 session: &mut ReviewSession,
                 watcher: &mut WebWatcher,
                 baseline: &mut review::LiveStateHandle,
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
    let mut baseline = review::LiveStateHandle::new(watcher.state_path.clone(), session.to_state());
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
fn tui_and_web_focus_adapters_share_target_and_error_contract() {
    let session = presentation_fixture();
    let cases = [
        ("a.rs", 2, Some(3), Ok(())),
        (
            "missing.rs",
            1,
            None,
            Err((-32602, "path is not in the diff: missing.rs")),
        ),
        (
            "a.rs",
            3,
            Some(2),
            Err((-32602, "end_line must be greater than or equal to line")),
        ),
        (
            "a.rs",
            99,
            None,
            Err((-32602, "location is not in the diff: a.rs:99")),
        ),
    ];

    for (path, line, end_line, expected) in cases {
        let tui = crate::tui::resolve_present_focus(&session, path, line, end_line);
        let web = resolve_present_focus(&session, path, line, end_line);
        assert_eq!(tui, web, "adapter drift for {path}:{line}");
        match expected {
            Ok(()) => {
                let target = tui.unwrap();
                assert_eq!(
                    serde_json::to_value(target.status).unwrap(),
                    json!({"ok":true,"path":"a.rs","line":2,"end_line":3})
                );
            }
            Err((code, message)) => {
                let actual = tui.unwrap_err().into_rpc();
                assert_eq!(actual.0, code);
                assert_eq!(actual.1, message);
            }
        }
    }
}

#[test]
fn most_recent_connected_tab_deterministically_controls_busy_and_current_focus() {
    let mut tabs = WebInteractions::default();
    tabs.connect("tab-a").unwrap();
    tabs.connect("tab-b").unwrap();
    tabs.report(InteractionReport {
        tab_id: "tab-a".into(),
        client_sequence: 1,
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
        client_sequence: 1,
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
    let mut baseline = review::LiveStateHandle::new(watcher.state_path.clone(), session.to_state());
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
            client_sequence: 1,
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
            client_sequence: 1,
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
fn salience_browser_model_selects_only_the_authoritative_exact_dom_target() {
    let target_rows = COMPONENT_JS
        .split("const targetRows = (target) => {")
        .nth(1)
        .and_then(|tail| tail.split("const materializeTarget").next())
        .expect("targetRows browser model");
    for phrase in [
        "CSS.escape(target.path || \"\")",
        ".diff-row[data-path=\"${escaped}\"]",
        "if (!Number.isSafeInteger(target.line)) return true",
        "target.line <= oldLine && oldLine <= end",
        "target.line <= newLine && newLine <= end",
    ] {
        assert!(
            target_rows.contains(phrase),
            "missing exact DOM match: {phrase}"
        );
    }

    let result_handler = COMPONENT_JS
        .split("[\"salience-promote\", \"salience-demote\", \"salience-set\", \"salience-clear\"]")
        .nth(1)
        .and_then(|tail| tail.split("}).then((result) => {").nth(1))
        .and_then(|tail| tail.split("      });").next())
        .expect("authoritative salience result handler");
    assert!(result_handler.contains("targetRows(result.target)"));
    assert!(!result_handler.contains("web-selected"));
    assert!(!result_handler.contains("button.closest"));
    assert!(!result_handler.contains("|| target"));
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
        "client_sequence: clientSequence",
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
    let walkthrough_shell = render_shell(&http_state(WebReview::from_session(
        &presentation_fixture(),
    )));
    assert!(walkthrough_shell.contains("data-action=\"walkthrough-goto\""));
    assert!(walkthrough_shell.contains("aria-label=\"Go to a specific Spotlight\""));
    assert!(walkthrough_shell.contains("data-step-id=\"first\""));
}

#[test]
fn interaction_reports_require_an_sse_lease_and_are_sequence_ordered() {
    let mut tabs = WebInteractions::default();
    assert!(
        tabs.report(InteractionReport {
            tab_id: "bad tab".into(),
            client_sequence: 1,
            focus: None,
            busy: None
        })
        .is_err()
    );
    assert!(
        tabs.report(InteractionReport {
            tab_id: "ok".into(),
            client_sequence: 1,
            focus: None,
            busy: Some("privileged-action".into())
        })
        .is_err()
    );
    assert_eq!(
        tabs.report(InteractionReport {
            tab_id: "before-sse".into(),
            client_sequence: 1,
            focus: None,
            busy: Some("search".into()),
        }),
        Ok(false),
        "an interaction POST must not create or connect a tab"
    );
    assert!(!tabs.tabs.contains_key("before-sse"));

    let old_connection = tabs.connect("ok").unwrap();
    assert_eq!(
        tabs.report(InteractionReport {
            tab_id: "ok".into(),
            client_sequence: 2,
            focus: None,
            busy: Some("search".into()),
        }),
        Ok(true)
    );
    assert_eq!(
        tabs.report(InteractionReport {
            tab_id: "ok".into(),
            client_sequence: 1,
            focus: None,
            busy: None,
        }),
        Ok(false),
        "a reordered clear must not overwrite newer busy state"
    );
    assert_eq!(tabs.tabs["ok"].busy.as_deref(), Some("search"));
    assert_eq!(
        tabs.report(InteractionReport {
            tab_id: "ok".into(),
            client_sequence: 3,
            focus: None,
            busy: None,
        }),
        Ok(true)
    );
    assert!(tabs.tabs["ok"].busy.is_none(), "busy eventually clears");
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
    assert_eq!(
        tabs.report(InteractionReport {
            tab_id: "ok".into(),
            client_sequence: 4,
            focus: None,
            busy: Some("search".into()),
        }),
        Ok(false),
        "a delayed keepalive after disconnect must not resurrect a tab"
    );
    assert!(tabs.controlling().is_none());

    let reconnect = tabs.connect("ok").unwrap();
    assert_eq!(
        tabs.report(InteractionReport {
            tab_id: "ok".into(),
            client_sequence: 5,
            focus: None,
            busy: None,
        }),
        Ok(true)
    );
    tabs.disconnect("ok", new_connection);
    assert_eq!(tabs.tabs["ok"].connection, reconnect);
    tabs.disconnect("ok", reconnect);
    assert!(tabs.tabs.is_empty());
    assert!(valid_tab_id("550e8400-e29b-41d4-a716-446655440000"));
    assert!(!valid_tab_id(&"x".repeat(129)));
}

#[test]
fn duplicated_tabs_with_copied_storage_have_independent_document_leases() {
    // Browser storage is irrelevant to identity: each loaded document
    // creates a distinct in-memory id and starts its own sequence at zero.
    assert!(!COMPONENT_JS.contains("gander.tabId"));
    assert!(!COMPONENT_JS.contains("gander.clientSequence"));
    assert!(COMPONENT_JS.contains("browserCrypto.randomUUID"));
    assert!(COMPONENT_JS.contains("browserCrypto.getRandomValues"));
    assert!(!COMPONENT_JS.contains("Math.random"));
    assert!(COMPONENT_JS.contains("let clientSequence = 0"));

    let mut tabs = WebInteractions::default();
    let a_first = tabs.connect("document-a").unwrap();
    let b = tabs.connect("document-b").unwrap();
    for id in ["document-a", "document-b"] {
        assert_eq!(
            tabs.report(InteractionReport {
                tab_id: id.into(),
                client_sequence: 1,
                focus: None,
                busy: None,
            }),
            Ok(true)
        );
    }
    let a_reconnect = tabs.connect("document-a").unwrap();
    tabs.disconnect("document-a", a_first);
    assert_eq!(tabs.tabs["document-a"].connection, a_reconnect);
    assert_eq!(tabs.tabs["document-a"].client_sequence, 1);
    tabs.disconnect("document-b", b);
    assert!(tabs.tabs.contains_key("document-a"));
    assert!(!tabs.tabs.contains_key("document-b"));
    tabs.disconnect("document-a", a_reconnect);

    // A reload is another document/lease and can safely restart at one.
    let reloaded = tabs.connect("document-a-reloaded").unwrap();
    assert!(
        tabs.report(InteractionReport {
            tab_id: "document-a-reloaded".into(),
            client_sequence: 1,
            focus: None,
            busy: None,
        })
        .unwrap()
    );
    assert_eq!(
        tabs.report(InteractionReport {
            tab_id: "document-a".into(),
            client_sequence: 2,
            focus: None,
            busy: None,
        }),
        Ok(false)
    );
    tabs.disconnect("document-a-reloaded", reloaded);
    assert!(tabs.tabs.is_empty());
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
