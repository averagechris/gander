use super::*;

pub(super) const INITIAL_REGION_WINDOW: usize = 4;
pub(super) const WATCH_TICK: Duration = Duration::from_millis(250);
pub(super) const REPO_POLL_INTERVAL: Duration = Duration::from_secs(2);
pub(super) const SSE_KEEPALIVE: Duration = Duration::from_secs(15);
pub(super) const SSE_BROADCAST_CAPACITY: usize = 16;
pub(super) const WATCH_JJ_TIMEOUT: Duration = Duration::from_secs(15);
pub(super) const WORKER_JOIN_TIMEOUT: Duration = Duration::from_secs(1);
/// Maximum time process shutdown allows existing HTTP requests to finish.
///
/// Claimed mutations remain authoritative during ordinary request handling and
/// throughout this drain. Once process shutdown exceeds this independent
/// bound, all accepted sockets are shut down and the Axum server future is
/// dropped so endpoint and worker cleanup cannot be held hostage by a request.
pub(super) const HTTP_GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
pub(super) const ACTION_RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
/// Loopback is single-user. This leaves ample room for browser parallelism and
/// SSE while bounding unauthenticated sockets before HTTP middleware runs.
pub(super) const MAX_HTTP_CONNECTIONS: usize = 64;

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
pub(super) struct HttpState {
    pub(super) token: Arc<str>,
    pub(super) expected_host: Arc<str>,
    pub(super) expected_origin: Arc<str>,
    pub(super) summary: Arc<str>,
    pub(super) base: Arc<str>,
    pub(super) rev: Arc<str>,
    pub(super) target: Arc<str>,
    pub(super) theme_css: Arc<str>,
    pub(super) style_nonce: Arc<str>,
    pub(super) content_security_policy: Arc<str>,
    pub(super) worker_healthy: Arc<AtomicBool>,
    pub(super) review: Arc<RwLock<WebReview>>,
    pub(super) recovery: Arc<RwLock<Arc<ProjectionEvent>>>,
    pub(super) events: tokio::sync::broadcast::Sender<Arc<ProjectionEvent>>,
    pub(super) present_events: tokio::sync::watch::Receiver<Option<Arc<PresentEvent>>>,
    pub(super) interactions: Arc<Mutex<WebInteractions>>,
    pub(super) shutdown: tokio::sync::watch::Receiver<bool>,
    pub(super) extra_css: Option<Arc<str>>,
    pub(super) heartbeat: RegistryHeartbeat,
    pub(super) actions: SyncSender<ActionEnvelope>,
}

#[derive(Debug)]
pub(super) struct ActionEnvelope {
    pub(super) expected_generation: u64,
    pub(super) command: ActionCommand,
    pub(super) response:
        tokio::sync::oneshot::Sender<std::result::Result<ActionResult, ActionError>>,
    pub(super) lifecycle: Arc<ActionLifecycle>,
}

/// Monotonic ownership of an action from admission through execution.
#[derive(Debug)]
pub(super) struct ActionLifecycle(pub(super) AtomicU8);

pub(super) const ACTION_QUEUED: u8 = 0;
pub(super) const ACTION_RUNNING: u8 = 1;
pub(super) const ACTION_COMPLETED: u8 = 2;
pub(super) const ACTION_CANCELLED: u8 = 3;

impl ActionLifecycle {
    pub(super) fn queued() -> Self {
        Self(AtomicU8::new(ACTION_QUEUED))
    }

    pub(super) fn claim(&self) -> bool {
        self.0
            .compare_exchange(
                ACTION_QUEUED,
                ACTION_RUNNING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn cancel_queued(&self) -> bool {
        self.0
            .compare_exchange(
                ACTION_QUEUED,
                ACTION_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn complete(&self) {
        self.0
            .compare_exchange(
                ACTION_RUNNING,
                ACTION_COMPLETED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .expect("only a claimed action can complete");
    }
}

#[derive(Debug)]
pub(super) enum ActionCommand {
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
pub(super) enum SalienceVerb {
    Set,
    Clear,
    Promote,
    Demote,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum WalkthroughVerb {
    Start,
    Next,
    Prev,
    Goto,
}

#[derive(Debug, Serialize)]
pub(super) struct ActionResult {
    pub(super) generation: u64,
    pub(super) result: Value,
}

#[derive(Debug)]
pub(super) struct ActionError {
    pub(super) status: StatusCode,
    pub(super) message: String,
    pub(super) rollback_safe: bool,
}

impl ActionError {
    pub(super) fn bad(error: impl std::fmt::Display) -> Self {
        let message = error.to_string();
        let status = if message.starts_with("unknown ") || message.contains("not found") {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        };
        Self {
            status,
            message,
            rollback_safe: true,
        }
    }
    pub(super) fn conflict(current: u64) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: format!("stale action generation; current generation is {current}"),
            rollback_safe: true,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GenerationAction {
    pub(super) expected_generation: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileViewedAction {
    pub(super) expected_generation: u64,
    pub(super) path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AcknowledgeAction {
    pub(super) expected_generation: u64,
    #[serde(default)]
    pub(super) fold_id: Option<String>,
    #[serde(default)]
    pub(super) path: Option<String>,
    #[serde(default)]
    pub(super) line: Option<usize>,
    #[serde(default)]
    pub(super) end_line: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TargetAction {
    pub(super) path: String,
    #[serde(default)]
    pub(super) line: Option<usize>,
    #[serde(default)]
    pub(super) end_line: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CommentAddAction {
    pub(super) expected_generation: u64,
    #[serde(default)]
    pub(super) path: Option<String>,
    #[serde(default)]
    pub(super) line: Option<usize>,
    #[serde(default)]
    pub(super) end_line: Option<usize>,
    pub(super) body: String,
    #[serde(default)]
    pub(super) kind: Option<CommentKind>,
    #[serde(default)]
    pub(super) action: Option<ActionIntent>,
    #[serde(default)]
    pub(super) state: Option<CommentState>,
    #[serde(default)]
    pub(super) channel: Option<Channel>,
    #[serde(default)]
    pub(super) source_comment_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CommentEditAction {
    pub(super) expected_generation: u64,
    pub(super) id: String,
    #[serde(default)]
    pub(super) body: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_patch")]
    pub(super) kind: Option<Option<CommentKind>>,
    #[serde(default, deserialize_with = "deserialize_optional_patch")]
    pub(super) action: Option<Option<ActionIntent>>,
    #[serde(default)]
    pub(super) channel: Option<Channel>,
}

pub(super) fn deserialize_optional_patch<'de, D, T>(
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
pub(super) struct CommentReplyAction {
    pub(super) expected_generation: u64,
    pub(super) id: String,
    pub(super) body: String,
    #[serde(default)]
    pub(super) resolve: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CommentStateAction {
    pub(super) expected_generation: u64,
    pub(super) id: String,
    pub(super) state: CommentState,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DraftAcceptAction {
    pub(super) expected_generation: u64,
    pub(super) id: String,
    #[serde(default)]
    pub(super) body: Option<String>,
    #[serde(default)]
    pub(super) channel: Option<Channel>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IdAction {
    pub(super) expected_generation: u64,
    pub(super) id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SalienceAction {
    pub(super) expected_generation: u64,
    pub(super) target: TargetAction,
    #[serde(default)]
    pub(super) salience: Option<Salience>,
    #[serde(default)]
    pub(super) rationale: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WalkthroughGotoAction {
    pub(super) expected_generation: u64,
    pub(super) step_id: String,
    #[serde(default)]
    pub(super) part: Option<usize>,
}

pub(super) type WebReview = GuideView;

#[derive(Debug, Clone)]
pub(super) struct RegionPatch {
    pub(super) id: String,
    /// Mode-independent markup, used only by compact recovery events.
    pub(super) html: Option<String>,
    pub(super) guided: Option<String>,
    pub(super) full: Option<String>,
    pub(super) remove: bool,
}

#[derive(Debug, Clone)]
pub(super) struct ProjectionEvent {
    pub(super) generation: u64,
    pub(super) full: bool,
    pub(super) patches: Vec<RegionPatch>,
    pub(super) order: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct RenderedProjection {
    pub(super) regions: BTreeMap<String, (String, String)>,
    pub(super) recovery_patches: Vec<RegionPatch>,
    pub(super) order: Vec<String>,
}

impl RenderedProjection {
    pub(super) fn effective_content_eq(&self, other: &Self) -> bool {
        self.order == other.order
            && self
                .regions
                .iter()
                .filter(|(id, _)| id.as_str() != "footer")
                .eq(other
                    .regions
                    .iter()
                    .filter(|(id, _)| id.as_str() != "footer"))
    }

    pub(super) fn refresh_footer(&mut self, review: &WebReview) {
        let footer = render_footer(review);
        self.regions
            .insert("footer".into(), (footer.clone(), footer.clone()));
        if let Some(patch) = self
            .recovery_patches
            .iter_mut()
            .find(|patch| patch.id == "footer")
        {
            patch.html = Some(footer);
        }
    }
}

/// Latest accepted browser interaction awaiting the blocking coordinator.
/// Replacing the timestamp makes mouse/scroll storms constant-space while the
/// coordinator is busy with jj, projection, or rendering work.
#[derive(Debug, Clone, Default)]
pub(super) struct RegistryHeartbeat(pub(super) Arc<Mutex<Option<DateTime<Utc>>>>);

impl RegistryHeartbeat {
    pub(super) fn record(&self, at: DateTime<Utc>) {
        if let Ok(mut pending) = self.0.lock()
            && pending.is_none_or(|current| at > current)
        {
            *pending = Some(at);
        }
    }

    pub(super) fn take(&self) -> Option<DateTime<Utc>> {
        self.0.lock().ok()?.take()
    }
}

pub(super) struct ReceiverStream<T> {
    pub(super) receiver: tokio::sync::mpsc::Receiver<T>,
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
