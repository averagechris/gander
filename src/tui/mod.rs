//! Ratatui/Crossterm terminal UI.
//!
//! Module layout:
//! - [`keymap`]: configurable key parsing and action lookup
//! - [`editor`]: the multiline comment editor widget state
//! - [`chooser`]: the base/tip target picker state and fuzzy filtering
//! - [`render`]: all drawing code (panes, popups, styles)
//!
//! This file owns the event loop, mode state machine, and event handling.

mod action_items;
mod annotation_card;
mod chooser;
mod comments;
mod drafts;
mod editor;
mod flags;
mod glance;
mod helpers;
mod keymap;
mod menu;
mod ops;
mod osc_guard;
mod outline;
#[cfg(all(test, unix))]
mod pty_tests;
mod render;
mod revset;
mod search;
mod text_layout;
pub(crate) mod theme;
mod view_options;
mod viewport;
mod walkthroughs;

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use color_eyre::eyre::{Context, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::{Backend, CrosstermBackend},
    layout::Rect,
};

use crate::{
    app::{CommentSelection, Focus, NavigationPlacement, ReviewSession, SkimAcknowledgeResult},
    artifact::{
        ArtifactBuildOptions, ArtifactProfile, ReviewArtifact, action_item_count,
        render_handoff_markdown,
    },
    clipboard::{ClipboardMethod, copy_to_clipboard},
    config::{KeybindingsConfig, ThemeConfig, ThemeModeConfig, UiConfig},
    diff::DiffSet,
    generated::GeneratedMatcher,
    jj::{JjBackend, JjChangeSummary, ReviewTarget},
    review,
    state::{AuthorKind, Channel, ReviewState, ReviewStateTombstones, WalkthroughStep},
};
use serde_json::{Value, json};

use action_items::{OpenWorkListState, OpenWorkRow};
use chooser::TargetChooserState;
use comments::CommentListState;
use drafts::DraftListState;
use editor::CommentEditor;
use flags::FlagListState;
use glance::GlanceBoardState;
use helpers::JjHelperState;
use keymap::{Action, KeyContext, KeyMap};
use ops::OperationPickerState;
use osc_guard::OscTailGuard;
use outline::SymbolOutlineState;
#[cfg(test)]
use render::diff_cursor_is_visible;
use render::{
    comment_editor_inner, diff_hit_at_point, downgrade_diff_theme, draw,
    ensure_diff_cursor_visible, inner_bordered, point_in_rect, row_in_inner,
    scroll_diff_horizontal_visual, scroll_diff_to_bottom_visual, scroll_diff_visual,
    terminal_supports_truecolor, ui_layout,
};
use revset::RevsetInputState;
use search::FileSearchState;
use theme::{AppTheme, BackgroundDetection};
use view_options::{ViewOption, ViewOptionsState};
use walkthroughs::WalkthroughListState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ActivityListState {
    selected: usize,
}

impl ActivityListState {
    fn new() -> Self {
        Self { selected: 0 }
    }
    fn move_selection(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected as isize + delta).clamp(0, len as isize - 1) as usize;
    }
}

enum Mode {
    Normal,
    Help,
    TargetChooser(TargetChooserState),
    RevsetInput(RevsetInputState),
    OperationPicker(OperationPickerState),
    JjHelpers(JjHelperState),
    FlagList(FlagListState),
    OpenWork(OpenWorkListState),
    Activity(ActivityListState),
    DraftList(DraftListState),
    FileSearch(FileSearchState),
    SymbolOutline(SymbolOutlineState),
    CommentList(CommentListState),
    ViewOptions(ViewOptionsState),
    AttentionGlance(GlanceBoardState),
    WalkthroughList(WalkthroughListState),
    CommentInput {
        editor: CommentEditor,
        target: CommentInputTarget,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewPointerPolicy {
    Enabled,
    BlockedByModal,
}

impl Mode {
    /// Whether mouse input may reach the review surface behind this mode.
    /// Keep this exhaustive: adding a modal must make its pointer ownership an
    /// explicit decision rather than inheriting review mutations by omission.
    fn review_pointer_policy(&self) -> ReviewPointerPolicy {
        match self {
            Self::Normal => ReviewPointerPolicy::Enabled,
            Self::Help
            | Self::TargetChooser(_)
            | Self::RevsetInput(_)
            | Self::OperationPicker(_)
            | Self::JjHelpers(_)
            | Self::FlagList(_)
            | Self::OpenWork(_)
            | Self::Activity(_)
            | Self::DraftList(_)
            | Self::FileSearch(_)
            | Self::SymbolOutline(_)
            | Self::CommentList(_)
            | Self::ViewOptions(_)
            | Self::AttentionGlance(_)
            | Self::WalkthroughList(_)
            | Self::CommentInput { .. } => ReviewPointerPolicy::BlockedByModal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CommentInputTarget {
    New,
    NewGeneral,
    Edit {
        id: String,
    },
    /// Editing an agent draft before accepting it as a comment.
    AcceptDraft {
        id: String,
    },
}

#[derive(Debug)]
struct TuiState {
    diff_viewport: viewport::DiffViewportController,
    help_scroll: usize,
    terminal_size: ratatui::prelude::Size,
    launch_target: Option<ReviewTarget>,
    diff_drag: Option<DiffDrag>,
    notice: Option<UiNotice>,
    /// Durable-state generation at the last autosave. Autosave compares this
    /// against [`ReviewSession::durable_state_generation`] and skips every
    /// serialization/fingerprint/stat step when nothing durable changed, so
    /// an unchanged session costs O(1) per event.
    last_autosave_generation: Option<u64>,
    /// Modification time of the agent overlay at the last poll, so agent
    /// suggestions written mid-session are picked up without reloading on
    /// every tick.
    overlay_mtime: Option<std::time::SystemTime>,
    state_mtime: Option<std::time::SystemTime>,
    /// Exact durable snapshot this live instance last loaded or persisted.
    /// Autosave computes a delta from this baseline instead of writing the
    /// whole in-memory snapshot back over concurrent writers.
    last_persisted_state: Option<ReviewState>,
    state_tombstones: ReviewStateTombstones,
    /// Where the agent overlay lives, for writing draft dispositions back.
    agent_overlay_path: Option<PathBuf>,
    /// The live ACP bridge has handled at least one agent/harness request for
    /// this TUI session. Socket existence alone is not attachment evidence.
    agent_contacted: bool,
    /// This instance's registry entry; heartbeats on input, removed on drop.
    instance_registration: Option<crate::registry::InstanceRegistration>,
    /// Live presenter cursor over durable Spotlight ordering. Presentation
    /// drives the normal stream and Focus preset; it never owns a modal view.
    presentation: Option<PresentationState>,
    /// Last repo poll for live refresh, throttled to [`REPO_POLL_INTERVAL`].
    last_repo_poll: Option<std::time::Instant>,
    /// `(target, fingerprint)` of the reviewed range at the last poll; a
    /// fingerprint change for the same target means new work landed and the
    /// review should refresh in place.
    repo_fingerprint: Option<(String, String)>,
    current_identity_chip: Option<String>,
    activity: VecDeque<ActivityEvent>,
    /// Consecutive fingerprint-poll failures; surfaces a footer warning at
    /// [`FINGERPRINT_FAILURE_NOTICE_THRESHOLD`] so a broken watcher cannot
    /// freeze silently behind a "following @" indicator.
    fingerprint_failures: u32,
    /// The derived chrome theme all rendering resolves through
    /// (docs/roadmap.md M19). Defaults to dark/truecolor for tests.
    theme: AppTheme,
    /// Containment for stray OSC 11 replies after any non-`Detected`
    /// auto-background query outcome; inactive for parsed replies and when no
    /// query was sent.
    osc_guard: OscTailGuard,
    layout_config: UiConfig,
    file_pane: FilePaneState,
    /// Open menu-bar dropdown + hovered item. View chrome, never a Mode:
    /// opening any modal closes it and Esc treats it as the topmost
    /// transient layer.
    menu: menu::MenuUiState,
    /// Ephemeral attention Focus preset. This is view state, never a Mode or
    /// durable review phase, so normal review dispatch remains untouched.
    attention_focus: Option<AttentionFocusState>,
}

impl Default for TuiState {
    fn default() -> Self {
        let layout_config = UiConfig::default();
        Self {
            diff_viewport: viewport::DiffViewportController::default(),
            help_scroll: 0,
            terminal_size: ratatui::prelude::Size::default(),
            launch_target: None,
            diff_drag: None,
            notice: None,
            last_autosave_generation: None,
            overlay_mtime: None,
            state_mtime: None,
            last_persisted_state: None,
            state_tombstones: ReviewStateTombstones::default(),
            agent_overlay_path: None,
            agent_contacted: false,
            instance_registration: None,
            presentation: None,
            last_repo_poll: None,
            repo_fingerprint: None,
            current_identity_chip: None,
            activity: VecDeque::new(),
            fingerprint_failures: 0,
            theme: AppTheme::default(),
            osc_guard: OscTailGuard::default(),
            file_pane: FilePaneState {
                explicit_override: None,
                split_percent: layout_config.file_pane_split_percent,
            },
            menu: menu::MenuUiState::default(),
            attention_focus: None,
            layout_config,
        }
    }
}

#[derive(Debug, Clone)]
struct AttentionFocusState {
    target_key: String,
    prior_file_pane: FilePaneState,
    prior_file_pane_visible: bool,
    prior_app_view: crate::app::FocusViewSnapshot,
    prior_viewport: viewport::ControllerTransactionSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PresentationState {
    identity: SpotlightIdentity,
    index: usize,
    stale: bool,
    /// Whether presentation entered Focus and therefore owns restoring it on
    /// explicit end. A pre-existing user Focus remains active after end.
    owns_focus: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpotlightIdentity {
    step_id: String,
    part: usize,
}

struct ActiveAttentionFocusRefresh {
    session: ReviewSession,
    viewport_transaction: viewport::ControllerTransactionSnapshot,
    viewport_refresh: viewport::RefreshSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FilePaneState {
    explicit_override: Option<bool>,
    split_percent: u16,
}

impl Default for FilePaneState {
    fn default() -> Self {
        Self {
            explicit_override: None,
            split_percent: UiConfig::default().file_pane_split_percent,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EffectiveFilePane {
    visible: bool,
    split_percent: u16,
}

impl TuiState {
    /// Resolve the one effective file-pane state used by every geometry,
    /// rendering, focus, and input path. Focus hides the pane; otherwise the
    /// explicit user override wins and the persisted preference is subject to the
    /// configured responsive breakpoint at the actual terminal width.
    fn effective_file_pane(
        &self,
        session: &ReviewSession,
        terminal_width: u16,
    ) -> EffectiveFilePane {
        let responsive_preference = session.file_pane_visible
            && terminal_width >= self.layout_config.file_pane_auto_hide_width;
        EffectiveFilePane {
            visible: if self.attention_focus.is_some() {
                false
            } else {
                self.file_pane
                    .explicit_override
                    .unwrap_or(responsive_preference)
            },
            split_percent: self.file_pane.split_percent.clamp(10, 60),
        }
    }

    fn review_layout(&self, session: &ReviewSession, area: Rect) -> render::UiLayout {
        let pane = self.effective_file_pane(session, area.width);
        ui_layout(area, pane.visible, &self.layout_config, pane.split_percent)
    }

    fn correct_file_pane_focus(&self, session: &mut ReviewSession, terminal_width: u16) {
        if !self.effective_file_pane(session, terminal_width).visible
            && session.focus == Focus::Files
        {
            session.focus = Focus::Diff;
        }
    }

    fn toggle_file_pane(&mut self, session: &mut ReviewSession, terminal_width: u16) {
        let next = !self.effective_file_pane(session, terminal_width).visible;
        self.file_pane.explicit_override = Some(next);
        session.file_pane_visible = next;
        self.correct_file_pane_focus(session, terminal_width);
    }

    fn enter_attention_focus(&mut self, session: &mut ReviewSession) {
        debug_assert!(self.attention_focus.is_none());
        let selected_row_id = session.selected_stream_row().map(|row| row.id);
        // Capture every pre-preset coordinate before folding mutates the
        // stream projection or invalidates measured annotation geometry.
        let prior_viewport = self.diff_viewport.transaction_snapshot();
        let prior_file_pane = self.file_pane;
        let prior_file_pane_visible = session.file_pane_visible;
        let prior_app_view = session.capture_focus_view_and_fold();
        let state = AttentionFocusState {
            target_key: session.target.to_string(),
            prior_file_pane,
            prior_file_pane_visible,
            prior_app_view,
            prior_viewport,
        };
        self.attention_focus = Some(state);
        session.stream_mode = true;
        if let Some(id) = selected_row_id {
            session.reanchor_stream_cursor(&id);
        }
        session.focus = Focus::Diff;
        self.correct_file_pane_focus(session, self.terminal_size.width);
        let _ = self.diff_viewport.pin_current_spotlight(session);
        self.diff_viewport
            .place_cursor(session, current_diff_inner(session, self));
    }

    fn leave_attention_focus(&mut self, session: &mut ReviewSession) {
        let Some(state) = self.attention_focus.take() else {
            return;
        };
        self.file_pane = state.prior_file_pane;
        session.file_pane_visible = state.prior_file_pane_visible;
        if state.target_key == session.target.to_string() {
            session.restore_focus_view(state.prior_app_view);
            self.diff_viewport.restore_transaction(state.prior_viewport);
        } else {
            session.restore_focus_folding_from_view(state.prior_app_view);
            session.focus = Focus::Diff;
            self.diff_viewport.reset(session);
        }
        self.correct_file_pane_focus(session, self.terminal_size.width);
        self.diff_viewport
            .reflow(session, current_diff_inner(session, self), false);
    }

    fn toggle_attention_focus(&mut self, session: &mut ReviewSession) -> bool {
        if self.attention_focus.is_some() {
            self.leave_attention_focus(session);
            false
        } else {
            self.enter_attention_focus(session);
            true
        }
    }

    fn suspend_attention_focus_for_load(
        &mut self,
        session: &mut ReviewSession,
    ) -> Option<AttentionFocusState> {
        let state = self.attention_focus.take()?;
        self.file_pane = state.prior_file_pane;
        session.file_pane_visible = state.prior_file_pane_visible;
        session.restore_focus_folding_from_view(state.prior_app_view.clone());
        Some(state)
    }

    fn resume_attention_focus_after_load(
        &mut self,
        session: &mut ReviewSession,
        state: AttentionFocusState,
    ) {
        let _ = session.apply_maximum_attention_folding();
        self.attention_focus = Some(state);
        session.stream_mode = true;
        session.focus = Focus::Diff;
        self.correct_file_pane_focus(session, self.terminal_size.width);
        let _ = self.diff_viewport.pin_current_spotlight(session);
        self.diff_viewport
            .place_cursor(session, current_diff_inner(session, self));
    }

    fn suspend_attention_focus_for_refresh(
        &mut self,
        session: &mut ReviewSession,
    ) -> Option<ActiveAttentionFocusRefresh> {
        self.attention_focus.as_ref()?;
        let inner = current_diff_inner(session, self);
        let active = ActiveAttentionFocusRefresh {
            session: session.clone(),
            viewport_transaction: self.diff_viewport.transaction_snapshot(),
            viewport_refresh: self.diff_viewport.refresh_snapshot(session, inner),
        };
        self.leave_attention_focus(session);
        Some(active)
    }

    fn resume_attention_focus_after_refresh(
        &mut self,
        session: &mut ReviewSession,
        review_loader: &ReviewLoader<'_>,
        active: ActiveAttentionFocusRefresh,
        refreshed: bool,
    ) {
        self.enter_attention_focus(session);
        let mut active_session = active.session;
        if refreshed
            && review_loader
                .load_in_place(&mut active_session, session.target.clone())
                .is_err()
        {
            return;
        }
        session.restore_active_focus_view_from(&active_session);
        self.diff_viewport
            .restore_transaction(active.viewport_transaction);
        if refreshed {
            self.diff_viewport.refreshed(
                active.viewport_refresh,
                session,
                current_diff_inner(session, self),
            );
        } else {
            self.diff_viewport
                .reflow(session, current_diff_inner(session, self), false);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActivityEvent {
    timestamp: chrono::DateTime<chrono::Utc>,
    key: String,
    message: String,
    count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UiNotice {
    level: UiNoticeLevel,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiNoticeLevel {
    Info,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DiffDrag {
    start_row: usize,
    start_file_index: usize,
    current_row: usize,
    saw_drag: bool,
}

struct ReviewLoader<'a> {
    ignore_globs: Vec<String>,
    generated_matcher: GeneratedMatcher,
    jj: &'a dyn JjBackend,
}

/// Filesystem locations the TUI reads and writes during a session, resolved
/// by the caller (see [`crate::paths`]); `None` disables the corresponding
/// behavior (useful in tests).
#[derive(Debug, Clone, Default)]
pub struct TuiPaths {
    /// Durable review state (viewed marks, comments), autosaved per event.
    pub state_file: Option<PathBuf>,
    /// The shared agent overlay polled for suggestions.
    pub agent_overlay: Option<PathBuf>,
    /// Unix socket for the live ACP endpoint (per instance).
    pub acp_socket: Option<PathBuf>,
    /// Instance registry directory; when set (with `workspace_root`), the
    /// TUI advertises itself while running (docs/decisions.md D3).
    pub registry_dir: Option<PathBuf>,
    /// Canonicalized workspace root advertised in the registry.
    pub workspace_root: Option<PathBuf>,
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    session: &mut ReviewSession,
    keybindings: &KeybindingsConfig,
    theme_config: &ThemeConfig,
    ui_config: &UiConfig,
    ignore_globs: Vec<String>,
    generated_matcher: GeneratedMatcher,
    jj: &dyn JjBackend,
    // Owned backend handed to the live ACP endpoint so agents can query the
    // stack (`review/stack_changes`, `review/change_diff`).
    acp_jj: Option<Box<dyn JjBackend + Send>>,
    paths: TuiPaths,
    start_tour: bool,
) -> Result<()> {
    session.stream_mode = true;
    session.materialize_stream_file_reanchored(session.selected);
    let TuiPaths {
        state_file: state_path,
        agent_overlay: agent_overlay_path,
        acp_socket: acp_socket_path,
        registry_dir,
        workspace_root,
    } = paths;
    let keymap = KeyMap::try_from(keybindings)?;
    let truecolor = terminal_supports_truecolor();
    // Resolve the derived theme up front. `mode = "auto"` queries the
    // terminal background via OSC 11 exactly once, before Gander enables
    // crossterm raw mode or starts the crossterm event reader. The query
    // library temporarily uses and restores its own guarded raw mode
    // (docs/roadmap.md M19). Explicit modes never query.
    let detection = match theme_config.mode {
        ThemeModeConfig::Auto => theme::detect_terminal_background(theme::OSC_QUERY_TIMEOUT),
        ThemeModeConfig::Dark | ThemeModeConfig::Light => BackgroundDetection::Unsupported,
    };
    let detected_background = match detection {
        BackgroundDetection::Detected(rgb) => Some(rgb),
        BackgroundDetection::Unsupported | BackgroundDetection::Inconclusive => None,
    };
    let app_theme = AppTheme::resolve_config(theme_config, truecolor, detected_background);
    // Only a parsed reply proves every solicited byte was consumed. An
    // `Unsupported` verdict is *usually* fence-validated, but the query
    // library can also report it without having confirmed the DA1 fence
    // (for example a reply fragmented straight after its ESC), so anything
    // other than `Detected` arms containment (docs/theme.md).
    let osc_guard = if matches!(detection, BackgroundDetection::Detected(_)) {
        OscTailGuard::inactive()
    } else if matches!(theme_config.mode, ThemeModeConfig::Auto) {
        OscTailGuard::armed(std::time::Instant::now())
    } else {
        // Explicit modes never queried: nothing to contain.
        OscTailGuard::inactive()
    };
    // Explicitly configured truecolor cue specs quantize to indexed colors
    // on terminals that do not advertise 24-bit support
    // (docs/focused-diff-ux.md §1). Derived slots quantize inside AppTheme.
    if !truecolor {
        downgrade_diff_theme(&mut session.diff_cues.theme);
    }
    let review_loader = ReviewLoader {
        ignore_globs,
        generated_matcher,
        jj,
    };
    reload_stream_chapter_metadata(&review_loader, session);

    // Host a live ACP endpoint so agents can talk to this session while the
    // human reviews. The socket is per-instance (docs/decisions.md D3), and
    // a successfully bound instance advertises itself in the registry so
    // `gander acp`/`gander mcp` can route to it by cwd. Failure to bind
    // degrades to overlay-file collaboration with a footer notice.
    #[cfg(unix)]
    let (mut acp_bridge, acp_notice, instance_registration) =
        match (acp_socket_path, &agent_overlay_path) {
            (Some(socket_path), Some(overlay_path)) => {
                match crate::acp::socket::AcpBridge::bind(
                    socket_path.clone(),
                    overlay_path.clone(),
                    acp_jj,
                ) {
                    Ok(bridge) => {
                        let registration = match (&registry_dir, &workspace_root) {
                            (Some(registry_dir), Some(workspace_root)) => {
                                let now = chrono::Utc::now();
                                crate::registry::InstanceRegistration::register(
                                    registry_dir,
                                    crate::registry::InstanceInfo {
                                        pid: std::process::id(),
                                        workspace_root: workspace_root.clone(),
                                        base: session.target.base.clone(),
                                        rev: session.target.rev.clone(),
                                        summary: session.summary_line(),
                                        socket_path,
                                        started_at: now,
                                        last_input_at: now,
                                    },
                                )
                                .ok()
                            }
                            _ => None,
                        };
                        (Some(bridge), None, registration)
                    }
                    Err(error) => (None, Some(format!("acp socket unavailable: {error}")), None),
                }
            }
            _ => (None, None, None),
        };
    #[cfg(not(unix))]
    let acp_notice: Option<String> = {
        let _ = (acp_socket_path, acp_jj, registry_dir, workspace_root);
        None
    };

    enable_raw_mode()?;
    let raw_mode_guard = RawModeGuard::armed();
    // Render the interactive UI to stderr so stdout remains clean for artifacts.
    // This lets `gander > review.md` capture only the post-quit artifact.
    let mut stderr = io::stderr();
    enter_interactive_screen(&mut stderr)?;
    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;
    let initial_terminal_size = terminal.size()?;
    raw_mode_guard.disarm();
    let mut terminal_lifecycle = TerminalLifecycle::new(&mut terminal);
    let mut mode = Mode::Normal;

    // Seed the autosave generation so an unchanged session does not trigger
    // a write on the first event.
    let mut tui_state = TuiState {
        launch_target: Some(session.target.clone()),
        last_autosave_generation: Some(session.durable_state_generation()),
        agent_overlay_path: agent_overlay_path.clone(),
        state_mtime: state_path.as_deref().and_then(state_file_mtime),
        last_persisted_state: Some(session.to_state()),
        terminal_size: initial_terminal_size,
        theme: app_theme,
        osc_guard,
        layout_config: ui_config.clone(),
        file_pane: FilePaneState {
            split_percent: ui_config.file_pane_split_percent.clamp(10, 60),
            ..FilePaneState::default()
        },
        notice: acp_notice.map(|message| UiNotice {
            level: UiNoticeLevel::Info,
            message,
        }),
        ..TuiState::default()
    };
    // Correct focus before the first draw. The initial size is already stored,
    // so waiting for a Resize event would leave an auto-hidden pane focused.
    tui_state.correct_file_pane_focus(session, initial_terminal_size.width);
    #[cfg(unix)]
    {
        tui_state.instance_registration = instance_registration;
    }
    if start_tour {
        start_startup_tour_or_notice(session, &review_loader, &mut tui_state);
    }
    let result = run_loop(
        terminal_lifecycle.terminal_mut(),
        session,
        &mut mode,
        &keymap,
        &review_loader,
        &mut tui_state,
        state_path.as_deref(),
        agent_overlay_path.as_deref(),
        #[cfg(unix)]
        acp_bridge.as_mut(),
    );

    terminal_lifecycle.restore()?;
    result
}

struct RawModeGuard {
    armed: bool,
}

impl RawModeGuard {
    fn armed() -> Self {
        Self { armed: true }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = disable_raw_mode();
        }
    }
}

fn enter_interactive_screen<W: io::Write>(writer: &mut W) -> Result<()> {
    match execute!(writer, EnterAlternateScreen, EnableMouseCapture) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = execute!(writer, DisableMouseCapture, LeaveAlternateScreen);
            Err(error.into())
        }
    }
}

struct TerminalLifecycle<'a, B: Backend + io::Write>
where
    B::Error: Send + Sync + 'static,
{
    terminal: &'a mut Terminal<B>,
    restored: bool,
}

impl<'a, B: Backend + io::Write> TerminalLifecycle<'a, B>
where
    B::Error: Send + Sync + 'static,
{
    fn new(terminal: &'a mut Terminal<B>) -> Self {
        Self {
            terminal,
            restored: false,
        }
    }

    fn terminal_mut(&mut self) -> &mut Terminal<B> {
        self.terminal
    }

    fn restore(&mut self) -> Result<()> {
        restore_terminal(self.terminal)?;
        self.restored = true;
        Ok(())
    }
}

impl<B: Backend + io::Write> Drop for TerminalLifecycle<'_, B>
where
    B::Error: Send + Sync + 'static,
{
    fn drop(&mut self) {
        if !self.restored {
            let _ = restore_terminal(self.terminal);
        }
    }
}

fn restore_terminal<B: Backend + io::Write>(terminal: &mut Terminal<B>) -> Result<()>
where
    B::Error: Send + Sync + 'static,
{
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn start_startup_tour_or_notice(
    session: &mut ReviewSession,
    _review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) {
    if let Err((_, message)) = start_stream_presentation(session, tui_state) {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Info,
            message,
        });
    }
}

pub fn render_tour_text(
    session: &mut ReviewSession,
    keybindings: &KeybindingsConfig,
    jj: &dyn JjBackend,
    width: u16,
    height: u16,
    slide: Option<usize>,
) -> Result<String> {
    let keymap = KeyMap::try_from(keybindings)?;
    let _ = jj;
    let mut tui_state = TuiState {
        terminal_size: ratatui::prelude::Size::new(width, height),
        ..TuiState::default()
    };
    session.stream_mode = true;
    let total = session.spotlight_count();
    if total == 0 {
        return Ok("nothing to tour — no current Spotlight regions; author a durable walkthrough and attention map\n".to_owned());
    }
    tui_state.enter_attention_focus(session);
    // Every slide's destination is known up front; materialize them in one
    // stream rebuild instead of paying one full rebuild per jump.
    let spotlight_files = {
        let stream = session.review_stream();
        let mut indexes = Vec::new();
        for spotlight in &stream.spotlights {
            if let Some(path) = spotlight.target.file.as_deref()
                && let Some(index) = session.files.iter().position(|file| file.path == path)
                && !indexes.contains(&index)
            {
                indexes.push(index);
            }
        }
        indexes
    };
    session.materialize_stream_files(spotlight_files);
    let indices: Vec<usize> = match slide {
        Some(n) => vec![n.saturating_sub(1).min(total.saturating_sub(1))],
        None => (0..total).collect(),
    };
    let mut out = String::new();
    for idx in indices {
        let _ = session.jump_to_spotlight_index(idx);
        let _ = tui_state.diff_viewport.pin_current_spotlight(session);
        tui_state
            .diff_viewport
            .place_cursor(session, current_diff_inner(session, &tui_state));
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend)?;
        terminal
            .draw(|frame| render::draw(frame, session, &Mode::Normal, &keymap, &tui_state, None))?;
        let breadcrumb = session
            .selected_stream_row()
            .and_then(|row| row.path)
            .unwrap_or_else(|| "spotlight".to_owned());
        out.push_str(&format!("──── slide {}/{} ────\n", idx + 1, total));
        let slide_text = tour_buffer_text(
            terminal.backend().buffer(),
            &format!("slide {}/{} · {breadcrumb}", idx + 1, total),
        );
        out.push_str(&slide_text);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(out)
}

fn tour_buffer_text(buffer: &ratatui::buffer::Buffer, footer: &str) -> String {
    let area = buffer.area;
    let mut out = String::new();
    let content_height = area.height.saturating_sub(2);
    for y in area.y..area.y + content_height {
        let mut line = String::new();
        for x in area.x..area.x + area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.push_str(&footer.chars().take(area.width as usize).collect::<String>());
    out.push('\n');
    out
}

fn finish_ephemeral_views_on_quit(session: &mut ReviewSession, tui_state: &mut TuiState) {
    tui_state.presentation = None;
    if tui_state.attention_focus.is_some() {
        tui_state.leave_attention_focus(session);
    }
}

fn start_stream_presentation(
    session: &mut ReviewSession,
    tui_state: &mut TuiState,
) -> Result<(), (i64, String)> {
    if tui_state.presentation.is_some() {
        return Ok(());
    }
    session.stream_mode = true;
    if session.spotlight_count() == 0 {
        return Err((
            -32002,
            "nothing to present — no current Spotlight regions; author a durable walkthrough and attention map"
                .to_owned(),
        ));
    }
    let owns_focus = tui_state.attention_focus.is_none();
    if owns_focus {
        tui_state.enter_attention_focus(session);
    }
    let (step_id, part) = match session.jump_to_spotlight_index(0) {
        Some(identity) => identity,
        None => {
            if owns_focus {
                tui_state.leave_attention_focus(session);
            }
            return Err((-32002, "first Spotlight is unavailable".to_owned()));
        }
    };
    let _ = tui_state.diff_viewport.pin_current_spotlight(session);
    tui_state
        .diff_viewport
        .place_cursor(session, current_diff_inner(session, tui_state));
    tui_state.presentation = Some(PresentationState {
        identity: SpotlightIdentity { step_id, part },
        index: 0,
        stale: false,
        owns_focus,
    });
    tui_state.notice = Some(UiNotice {
        level: UiNoticeLevel::Info,
        message: format!(
            "presenting {} Spotlight(s) in the normal stream with Focus",
            session.spotlight_count()
        ),
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    session: &mut ReviewSession,
    mode: &mut Mode,
    keymap: &KeyMap,
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
    state_path: Option<&Path>,
    agent_overlay_path: Option<&Path>,
    #[cfg(unix)] mut acp_bridge: Option<&mut crate::acp::socket::AcpBridge>,
) -> Result<()> {
    if let Some(overlay_path) = agent_overlay_path {
        maybe_reload_agent_overlay(session, overlay_path, tui_state, review_loader, false);
    }
    // Opt-in frame-time instrumentation (`GANDER_FRAME_LOG=<path>`): appends
    // one line per handled event batch with handle+draw microseconds. When
    // the variable is unset this is a single `None` check per frame.
    let mut frame_log = FrameLog::from_env();
    // Large-change nudge: on a big review with no agent structure yet,
    // point at the collaboration affordances instead of leaving the human
    // to grind through the file list alone.
    if tui_state.notice.is_none()
        && let Some(nudge) = session.large_change_nudge()
    {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Info,
            message: nudge,
        });
    }
    loop {
        // Observe geometry before any queued command can place the viewport.
        // This is the single path that advances stored terminal dimensions;
        // stale Resize payloads are never allowed to move them backwards.
        observe_terminal_size(terminal.size()?, session, mode, tui_state);

        // Answer queued agent requests against the live session before
        // drawing so their effects render this frame.
        #[cfg(unix)]
        if let Some(bridge) = acp_bridge.as_deref_mut() {
            let (overlay_changed, had_requests, commands, mutations) =
                bridge.drain_ui_commands(session);
            tui_state.agent_contacted |= had_requests;
            for command in commands {
                let result = apply_present_command(
                    command.command.clone(),
                    review_loader,
                    session,
                    mode,
                    tui_state,
                    state_path,
                    agent_overlay_path,
                );
                command.respond(result);
            }
            for request in mutations {
                let result = apply_acp_review_mutation(
                    request.mutation.clone(),
                    session,
                    state_path,
                    tui_state,
                )
                .map_err(|error| (-32000, error.to_string()));
                request.respond(result);
            }
            if overlay_changed {
                // The bridge already applied overlay changes; skip the redundant
                // "file changed" reload+notice for our own writes.
                if let Some(overlay_path) = agent_overlay_path {
                    tui_state.overlay_mtime = std::fs::metadata(overlay_path)
                        .and_then(|metadata| metadata.modified())
                        .ok();
                }
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "agent suggestions updated".to_owned(),
                });
            }
        }

        let draw_started = frame_log.as_ref().map(|_| std::time::Instant::now());
        terminal.draw(|frame| {
            draw(
                frame,
                session,
                mode,
                keymap,
                tui_state,
                tui_state.notice.as_ref(),
            )
        })?;
        if let Some(log) = frame_log.as_mut()
            && let Some(started) = draw_started
        {
            log.record_draw(started.elapsed());
        }

        let admitted = if event::poll(Duration::from_millis(150))? {
            // Route every event through the OSC tail guard (a no-op unless
            // the startup background query had a non-Detected outcome): stray
            // OSC 11 reply fragments are contained, everything else dispatches
            // in order, possibly together with previously held events.
            tui_state
                .osc_guard
                .admit(event::read()?, std::time::Instant::now())
                .events
        } else {
            // Idle tick: first release any user input the guard was holding
            // so keystrokes are delayed by at most one poll interval.
            let held = tui_state.osc_guard.flush_idle(std::time::Instant::now());
            if held.is_empty() {
                // Idle ticks are the natural moment to pick up agent overlay
                // writes without competing with user input handling.
                if let Some(overlay_path) = agent_overlay_path {
                    maybe_reload_agent_overlay(
                        session,
                        overlay_path,
                        tui_state,
                        review_loader,
                        true,
                    );
                }
                if let Some(state_path) = state_path {
                    maybe_reload_review_state(session, state_path, tui_state, true);
                }
                // Live refresh: pick up new/rewritten changes while nothing
                // modal is open (a reload underneath a popup or comment editor
                // could misanchor what the human is doing).
                if mode_allows_live_refresh(mode) {
                    maybe_refresh_review(review_loader, session, tui_state);
                    if let Mode::OperationPicker(picker) = mode {
                        refresh_operation_picker_preview(picker, session, review_loader);
                    }
                }
                continue;
            }
            held
        };

        let handle_started = frame_log.as_ref().map(|_| std::time::Instant::now());
        let mut quit = false;
        for event in admitted {
            match event {
                Event::Key(key)
                    if handle_key_event(key, session, mode, keymap, review_loader, tui_state)? =>
                {
                    quit = true;
                    break;
                }
                Event::Key(_) => {}
                Event::Mouse(mouse) => {
                    match handle_menu_mouse_event(
                        mouse,
                        tui_state.terminal_size,
                        session,
                        mode,
                        keymap,
                        review_loader,
                        tui_state,
                    )? {
                        MenuMouseOutcome::Quit => {
                            quit = true;
                            break;
                        }
                        MenuMouseOutcome::Consumed => {}
                        MenuMouseOutcome::Ignored => handle_mouse_event(
                            mouse,
                            tui_state.terminal_size,
                            session,
                            mode,
                            tui_state,
                        ),
                    }
                }
                Event::Resize(width, height) => {
                    let queued = ratatui::prelude::Size::new(width, height);
                    let observed = terminal.size()?;
                    // Crossterm may leave older Resize events queued. Trust the
                    // backend's observed size and use the payload only when it
                    // still describes that same current terminal.
                    dispatch_resize_event(queued, observed, session, mode, tui_state);
                }
                _ => {}
            }
        }
        if quit {
            finish_ephemeral_views_on_quit(session, tui_state);
            if let Some(state_path) = state_path {
                autosave_state(session, state_path, tui_state);
            }
            break;
        }

        // Heartbeat the instance registry (throttled) so `current_focus` /
        // `list_reviews` can rank instances by recent human input, and keep
        // the advertised target/summary current after retargets.
        if let Some(registration) = tui_state.instance_registration.as_mut() {
            let _ = registration.record_input(
                &session.target.base,
                &session.target.rev,
                &session.summary_line(),
            );
        }

        // Persist viewed marks and comments as they change so a crash or
        // killed terminal cannot lose review progress (state is otherwise
        // only saved on clean quit).
        if let Some(state_path) = state_path {
            autosave_state(session, state_path, tui_state);
        }
        if let Some(log) = frame_log.as_mut()
            && let Some(started) = handle_started
        {
            log.record_handle(started.elapsed());
        }
    }
    Ok(())
}

/// Opt-in per-event frame-time log (`GANDER_FRAME_LOG=<path>`). Each handled
/// event batch appends `handle_us=<n> draw_us=<n>`, where `handle_us` covers
/// input dispatch through autosave and `draw_us` is the terminal draw that
/// rendered the result. Zero overhead when the variable is unset.
struct FrameLog {
    file: std::fs::File,
    pending_handle_us: Option<u128>,
}

impl FrameLog {
    fn from_env() -> Option<Self> {
        let path = std::env::var_os("GANDER_FRAME_LOG")?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(|file| Self {
                file,
                pending_handle_us: None,
            })
    }

    fn record_handle(&mut self, elapsed: std::time::Duration) {
        self.pending_handle_us = Some(elapsed.as_micros());
    }

    fn record_draw(&mut self, elapsed: std::time::Duration) {
        use std::io::Write as _;
        let Some(handle_us) = self.pending_handle_us.take() else {
            return;
        };
        let _ = writeln!(
            self.file,
            "handle_us={handle_us} draw_us={}",
            elapsed.as_micros()
        );
    }
}

fn resize_event_observed_size(
    queued: ratatui::prelude::Size,
    observed: ratatui::prelude::Size,
) -> ratatui::prelude::Size {
    if queued == observed { queued } else { observed }
}

fn dispatch_resize_event(
    queued: ratatui::prelude::Size,
    observed: ratatui::prelude::Size,
    session: &mut ReviewSession,
    mode: &mut Mode,
    tui_state: &mut TuiState,
) {
    observe_terminal_size(
        resize_event_observed_size(queued, observed),
        session,
        mode,
        tui_state,
    );
}

fn observe_terminal_size(
    observed: ratatui::prelude::Size,
    session: &mut ReviewSession,
    mode: &mut Mode,
    tui_state: &mut TuiState,
) {
    if tui_state.terminal_size != observed {
        let transition = tui_state
            .diff_viewport
            .transition_snapshot(session, current_diff_inner(session, tui_state));
        tui_state.terminal_size = observed;
        tui_state.correct_file_pane_focus(session, observed.width);
        resize_comment_editor(mode, observed);
        tui_state.diff_viewport.finish_transition(
            transition,
            session,
            current_diff_inner(session, tui_state),
        );
    } else {
        // Session preference can change independently of terminal dimensions
        // (retarget or Focus commands), so enforce this before every draw.
        tui_state.correct_file_pane_focus(session, observed.width);
        resize_comment_editor(mode, observed);
    }
}

fn resize_comment_editor(mode: &mut Mode, terminal_size: ratatui::prelude::Size) {
    let Mode::CommentInput { editor, .. } = mode else {
        return;
    };
    let inner = comment_editor_inner(Rect::new(0, 0, terminal_size.width, terminal_size.height));
    editor.resize(inner.width as usize, inner.height as usize);
}

fn yank_handoff(session: &ReviewSession, tui_state: &mut TuiState) {
    yank_handoff_with(session, tui_state, copy_to_clipboard);
}

fn yank_handoff_with(
    session: &ReviewSession,
    tui_state: &mut TuiState,
    mut copy: impl FnMut(&str) -> Result<ClipboardMethod>,
) {
    let options = ArtifactBuildOptions { only_open: false };
    let artifact = ReviewArtifact::build_with_options(session, ArtifactProfile::Agent, options);
    let count = action_item_count(&artifact);
    let todo_count = artifact
        .comments
        .iter()
        .filter(|comment| comment.comment.state == crate::state::CommentState::Todo)
        .count();
    let draft_count = artifact
        .comments
        .iter()
        .filter(|comment| comment.comment.state == crate::state::CommentState::Draft)
        .count();
    match render_handoff_markdown(session, options).and_then(|body| copy(&body)) {
        Ok(method) => {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: format!(
                    "handoff copied via {method} ({todo_count} todo comments included, {draft_count} drafts withheld; {count} action items)"
                ),
            });
        }
        Err(error) => {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: format!("failed to copy handoff: {error}"),
            });
        }
    }
}

/// Reload the agent overlay when its mtime changes, applying suggestions to
/// the session. `notify` controls whether a footer notice announces updates
/// (suppressed for the initial load).
fn maybe_reload_agent_overlay(
    session: &mut ReviewSession,
    overlay_path: &Path,
    tui_state: &mut TuiState,
    _review_loader: &ReviewLoader<'_>,
    notify: bool,
) {
    let mtime = std::fs::metadata(overlay_path)
        .and_then(|metadata| metadata.modified())
        .ok();
    if mtime.is_none() || mtime == tui_state.overlay_mtime {
        return;
    }
    tui_state.overlay_mtime = mtime;
    match crate::agent::AgentOverlay::load_or_default(overlay_path) {
        Ok(overlay) => {
            let transition = tui_state
                .diff_viewport
                .transition_snapshot(session, current_diff_inner(session, tui_state));
            session.apply_agent_overlay(&overlay);
            tui_state.diff_viewport.finish_transition(
                transition,
                session,
                current_diff_inner(session, tui_state),
            );
            if notify {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "agent ordering/flags updated".to_owned(),
                });
            }
        }
        Err(error) => {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: format!("failed to load agent overlay: {error}"),
            });
        }
    }
}

fn state_file_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn maybe_reload_review_state(
    session: &mut ReviewSession,
    state_path: &Path,
    tui_state: &mut TuiState,
    notify: bool,
) {
    let mtime = state_file_mtime(state_path);
    if mtime.is_none() || mtime == tui_state.state_mtime {
        return;
    }
    match ReviewState::load_or_default(state_path) {
        Ok(external) => {
            let transition = tui_state
                .diff_viewport
                .transition_snapshot(session, current_diff_inner(session, tui_state));
            let before_comments: BTreeSet<String> = session
                .comments
                .iter()
                .map(|comment| comment.id.clone())
                .collect();
            let local = session.to_state();
            let fallback = ReviewState::default();
            let base = tui_state.last_persisted_state.as_ref().unwrap_or(&fallback);
            let merged = ReviewState::merge_changes_since(
                external,
                base,
                local,
                &tui_state.state_tombstones,
            );
            let added_comments = merged
                .comments
                .iter()
                .filter(|comment| !before_comments.contains(&comment.id))
                .count();
            session.apply_review_state(merged.clone());
            tui_state.diff_viewport.finish_transition(
                transition,
                session,
                current_diff_inner(session, tui_state),
            );
            reconcile_present_spotlight(session, tui_state);
            tui_state.state_mtime = mtime;
            tui_state.last_persisted_state = Some(merged);
            tui_state.last_autosave_generation = Some(session.durable_state_generation());
            if notify && added_comments > 0 {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: format!(
                        "review state updated externally — {added_comments} comment{} added",
                        if added_comments == 1 { "" } else { "s" }
                    ),
                });
            }
        }
        Err(error) => {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: format!("failed to load review state: {error}"),
            });
        }
    }
}

/// How often the idle loop polls jj for new work in the reviewed range.
const REPO_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Consecutive fingerprint failures tolerated as transient before the
/// footer warns that live refresh is not working.
const FINGERPRINT_FAILURE_NOTICE_THRESHOLD: u32 = 5;

/// Poll the repo (throttled) and refresh the review in place when the
/// reviewed range changed underneath it — new changes landing, rewrites,
/// or working-copy edits. The poll first performs one explicit jj snapshot;
/// every follow-up query uses `--ignore-working-copy`, so live refresh has a
/// single, intentional op-log write point instead of letting each read-only
/// command implicitly snapshot. A dirty snapshot op can still be undone with
/// `jj undo`, which reverts those disk edits; avoid adding extra read-side
/// snapshots here unless jj gains a non-mutating dirty-tree fingerprint.
/// The first poll for a target only records the
/// baseline; view state is preserved across refreshes and comments/viewed
/// marks carry over by fingerprint, so the reload does not yank the
/// reviewer around.
fn maybe_refresh_review(
    review_loader: &ReviewLoader<'_>,
    session: &mut ReviewSession,
    tui_state: &mut TuiState,
) {
    let now = std::time::Instant::now();
    if tui_state
        .last_repo_poll
        .is_some_and(|last| now.duration_since(last) < REPO_POLL_INTERVAL)
    {
        return;
    }
    tui_state.last_repo_poll = Some(now);
    if let Err(error) = review_loader.jj.snapshot_working_copy(&session.repo) {
        tui_state.fingerprint_failures = tui_state.fingerprint_failures.saturating_add(1);
        if tui_state.fingerprint_failures == FINGERPRINT_FAILURE_NOTICE_THRESHOLD {
            let reason = error
                .to_string()
                .lines()
                .next()
                .unwrap_or("unknown error")
                .to_string();
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: format!("live refresh is failing — {reason}"),
            });
        }
        return;
    }
    // Transient jj failures (locks, mid-operation states) must not spam the
    // footer: skip the tick and try again. But a *persistently* failing poll
    // means the pane is frozen while claiming to follow the repo, so after a
    // run of consecutive failures surface it once as an error.
    let fingerprint = match review_loader
        .jj
        .change_fingerprint(&session.repo, &session.target)
    {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            tui_state.fingerprint_failures = tui_state.fingerprint_failures.saturating_add(1);
            if tui_state.fingerprint_failures == FINGERPRINT_FAILURE_NOTICE_THRESHOLD {
                let reason = error
                    .to_string()
                    .lines()
                    .next()
                    .unwrap_or("unknown error")
                    .to_string();
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Error,
                    message: format!("live refresh is failing — {reason}"),
                });
            }
            return;
        }
    };
    if tui_state.fingerprint_failures >= FINGERPRINT_FAILURE_NOTICE_THRESHOLD {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Info,
            message: "live refresh recovered".to_string(),
        });
    }
    tui_state.fingerprint_failures = 0;
    let target_key = session.target.to_string();
    let baseline = tui_state
        .repo_fingerprint
        .as_ref()
        .filter(|(target, _)| *target == target_key)
        .map(|(_, fingerprint)| fingerprint.clone());
    tui_state.repo_fingerprint = Some((target_key, fingerprint.clone()));
    match baseline {
        None => refresh_identity_chip(review_loader, session, tui_state),
        Some(previous) if previous == fingerprint => {}
        Some(previous) => {
            refresh_current_target(review_loader, session, tui_state, &previous, &fingerprint)
        }
    }
}

fn mode_allows_live_refresh(mode: &Mode) -> bool {
    matches!(
        mode,
        Mode::Normal | Mode::Activity(_) | Mode::Help | Mode::OperationPicker(_)
    )
}

fn mode_label(mode: &Mode) -> &'static str {
    match mode {
        Mode::Normal => "normal",
        Mode::Help => "help",
        Mode::TargetChooser(_) => "target chooser",
        Mode::RevsetInput(_) => "revset input",
        Mode::OperationPicker(_) => "operation picker",
        Mode::JjHelpers(_) => "jj helpers",
        Mode::FlagList(_) => "flag list",
        Mode::OpenWork(_) => "open work",
        Mode::Activity(_) => "activity",
        Mode::DraftList(_) => "draft list",
        Mode::FileSearch(_) => "file search",
        Mode::SymbolOutline(_) => "symbol outline",
        Mode::CommentList(_) => "comment list",
        Mode::ViewOptions(_) => "view options",
        Mode::AttentionGlance(_) => "attention glance",
        Mode::WalkthroughList(_) => "walkthrough list",
        Mode::CommentInput { .. } => "comment editor",
    }
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn apply_present_command(
    command: crate::acp::socket::PresentCommand,
    review_loader: &ReviewLoader<'_>,
    session: &mut ReviewSession,
    mode: &Mode,
    tui_state: &mut TuiState,
    state_path: Option<&Path>,
    agent_overlay_path: Option<&Path>,
) -> Result<Value, (i64, String)> {
    if !mode_allows_live_refresh(mode) {
        return Err((-32001, format!("user is busy: {}", mode_label(mode))));
    }
    use crate::acp::socket::PresentCommand;
    match command {
        PresentCommand::Status => Ok(present_status(session, tui_state)),
        PresentCommand::Start => {
            start_stream_presentation(session, tui_state)?;
            Ok(present_status(session, tui_state))
        }
        PresentCommand::End => {
            if let Some(presentation) = tui_state.presentation.take() {
                if presentation.owns_focus && tui_state.attention_focus.is_some() {
                    tui_state.leave_attention_focus(session);
                }
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "stream presentation ended".to_owned(),
                });
            }
            Ok(present_status(session, tui_state))
        }
        PresentCommand::Next => {
            let count = session.spotlight_count();
            let presentation = tui_state
                .presentation
                .as_ref()
                .ok_or_else(|| (-32002, "presentation is not active".to_owned()))?;
            if presentation.stale {
                return Err((
                    -32002,
                    "current Spotlight is stale; use present/goto to choose a current target"
                        .to_owned(),
                ));
            }
            let index = presentation.index;
            goto_present_spotlight(session, tui_state, (index + 1).min(count.saturating_sub(1)))?;
            Ok(present_status(session, tui_state))
        }
        PresentCommand::Prev => {
            let presentation = tui_state
                .presentation
                .as_ref()
                .ok_or_else(|| (-32002, "presentation is not active".to_owned()))?;
            if presentation.stale {
                return Err((
                    -32002,
                    "current Spotlight is stale; use present/goto to choose a current target"
                        .to_owned(),
                ));
            }
            let index = presentation.index;
            goto_present_spotlight(session, tui_state, index.saturating_sub(1))?;
            Ok(present_status(session, tui_state))
        }
        PresentCommand::GotoIndex(index) => {
            if tui_state.presentation.is_none() {
                return Err((-32002, "presentation is not active".to_owned()));
            }
            if index >= session.spotlight_count() {
                return Err((-32602, format!("slide index {index} out of range")));
            }
            goto_present_spotlight(session, tui_state, index)?;
            Ok(present_status(session, tui_state))
        }
        PresentCommand::GotoStep(step_id) => {
            if tui_state.presentation.is_none() {
                return Err((-32002, "presentation is not active".to_owned()));
            }
            let Some(index) = session.spotlight_index_for_step(&step_id) else {
                return Err((-32602, format!("unknown step_id: {step_id}")));
            };
            goto_present_spotlight(session, tui_state, index)?;
            Ok(present_status(session, tui_state))
        }
        PresentCommand::Focus {
            path,
            line,
            end_line,
            note,
        } => {
            if !session.files.iter().any(|file| file.path == path) {
                return Err((-32602, format!("path is not in the diff: {path}")));
            }
            let stream = session.review_stream();
            let row = stream.rows.iter().enumerate().find_map(|(index, row)| {
                if row.path.as_deref() != Some(path.as_str()) {
                    return None;
                }
                let anchor_line = row
                    .anchor
                    .as_ref()
                    .and_then(crate::anchor::CommentAnchor::line)?;
                let requested_end = end_line.unwrap_or(line);
                (line <= anchor_line && anchor_line <= requested_end).then_some(index)
            });
            drop(stream);
            let Some(row) = row else {
                return Err((
                    -32602,
                    format!("location is not in the diff: {path}:{line}"),
                ));
            };
            session.select_stream_row(row, true);
            session.focus = Focus::Diff;
            transition_to_logical_selection(session, tui_state);
            if let Some(note) = note {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: note,
                });
            }
            Ok(json!({ "ok": true, "path": path, "line": line, "end_line": end_line }))
        }
        PresentCommand::Reload => {
            if let Some(state_path) = state_path {
                maybe_reload_review_state(session, state_path, tui_state, false);
            }
            if let Some(agent_overlay_path) = agent_overlay_path {
                maybe_reload_agent_overlay(
                    session,
                    agent_overlay_path,
                    tui_state,
                    review_loader,
                    false,
                );
            }
            reconcile_present_spotlight(session, tui_state);
            Ok(present_status(session, tui_state))
        }
    }
}

fn present_status(session: &ReviewSession, tui_state: &TuiState) -> Value {
    let Some(presentation) = tui_state.presentation.as_ref() else {
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

fn goto_present_spotlight(
    session: &mut ReviewSession,
    tui_state: &mut TuiState,
    index: usize,
) -> Result<(), (i64, String)> {
    let (step_id, part) = session
        .jump_to_spotlight_index(index)
        .ok_or_else(|| (-32002, format!("Spotlight {index} is unavailable")))?;
    if let Some(presentation) = tui_state.presentation.as_mut() {
        presentation.identity = SpotlightIdentity { step_id, part };
        presentation.index = index;
        presentation.stale = false;
    }
    let _ = tui_state.diff_viewport.pin_current_spotlight(session);
    tui_state
        .diff_viewport
        .place_cursor(session, current_diff_inner(session, tui_state));
    Ok(())
}

fn reconcile_present_spotlight(session: &mut ReviewSession, tui_state: &mut TuiState) -> bool {
    let Some(identity) = tui_state
        .presentation
        .as_ref()
        .map(|presentation| presentation.identity.clone())
    else {
        return false;
    };
    let Some(index) = session.spotlight_index_for_identity(&identity.step_id, identity.part) else {
        if let Some(presentation) = tui_state.presentation.as_mut() {
            presentation.stale = true;
        }
        return false;
    };
    goto_present_spotlight(session, tui_state, index).is_ok()
}

/// Reload the current target in place: view state, durable attention, Focus,
/// and the presenter cursor survive and re-anchor conservatively.
fn refresh_current_target(
    review_loader: &ReviewLoader<'_>,
    session: &mut ReviewSession,
    tui_state: &mut TuiState,
    previous_fingerprint: &str,
    fingerprint: &str,
) {
    // Re-anchor the pre-preset viewport through refresh, then reapply Focus.
    // This avoids restoring a stale controller snapshot when Z is toggled off.
    let active_attention_focus = tui_state.suspend_attention_focus_for_refresh(session);
    let old_inner = current_diff_inner(session, tui_state);
    let viewport_snapshot = tui_state.diff_viewport.refresh_snapshot(session, old_inner);
    if let Err(error) = review_loader.load_in_place(session, session.target.clone()) {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Error,
            message: format!("failed to refresh review: {error:?}"),
        });
        if let Some(active) = active_attention_focus {
            tui_state.resume_attention_focus_after_refresh(session, review_loader, active, false);
        }
        return;
    }
    tui_state.diff_viewport.refreshed(
        viewport_snapshot,
        session,
        current_diff_inner(session, tui_state),
    );
    refresh_identity_chip(review_loader, session, tui_state);
    reapply_agent_overlay(session, review_loader, tui_state);
    let mut events = fingerprint_events(previous_fingerprint, fingerprint);
    if let Some(op) = latest_operation_description(review_loader, &session.repo) {
        for event in &mut events {
            event.push_str(&format!(" · op: {op}"));
        }
    }
    let mut file_events = session
        .last_refresh_changes
        .iter()
        .map(refresh_file_event)
        .collect::<Vec<_>>();
    specialize_description_only_events(&mut events, file_events.is_empty());
    let reviewed_changed = session
        .last_refresh_changes
        .iter()
        .filter(|change| change.was_reviewed)
        .count();
    events.append(&mut file_events);
    let mut message = if events.is_empty() {
        format!("repository changed — refreshed {}", session.target)
    } else {
        format!("repository changed — {}", events.join(" · "))
    };
    if reviewed_changed > 0 {
        let file_word = if reviewed_changed == 1 {
            "file"
        } else {
            "files"
        };
        message.push_str(&format!(
            " · {reviewed_changed} viewed {file_word} changed — needs re-review"
        ));
    }
    for event in events {
        push_activity(tui_state, event_key(&event), event);
    }
    while tui_state.activity.len() > 100 {
        tui_state.activity.pop_front();
    }
    tui_state.notice = Some(UiNotice {
        level: UiNoticeLevel::Info,
        message,
    });
    if let Some(active) = active_attention_focus {
        tui_state.resume_attention_focus_after_refresh(session, review_loader, active, true);
    }
    reconcile_present_spotlight(session, tui_state);
}

fn refresh_identity_chip(
    review_loader: &ReviewLoader<'_>,
    session: &ReviewSession,
    tui_state: &mut TuiState,
) {
    tui_state.current_identity_chip = review_loader
        .jj
        .stack_changes(&session.repo, &session.target)
        .ok()
        .and_then(|changes| changes.last().cloned())
        .map(|change| {
            let title = change.title().trim();
            format!(
                "@ {} {}",
                short_change_id(&change.change_id),
                if title.is_empty() {
                    "(no description)"
                } else {
                    title
                }
            )
        });
}

fn short_change_id(change_id: &str) -> &str {
    change_id.get(..8).unwrap_or(change_id)
}

fn latest_operation_description(
    review_loader: &ReviewLoader<'_>,
    repo: &std::path::Path,
) -> Option<String> {
    review_loader
        .jj
        .operations(repo)
        .ok()
        .and_then(|ops| ops.into_iter().next())
        .map(|op| humanize_operation_description(&op.description))
        .filter(|description| !description.trim().is_empty())
}

fn humanize_operation_description(description: &str) -> String {
    let mut out = String::with_capacity(description.len());
    for token in description.split_inclusive(char::is_whitespace) {
        let word_len = token.trim_end_matches(char::is_whitespace).len();
        let (word, suffix) = token.split_at(word_len);
        if word.len() == 128 && word.chars().all(|ch| ch.is_ascii_hexdigit()) {
            out.push_str(&word[..12]);
            out.push('…');
        } else {
            out.push_str(word);
        }
        out.push_str(suffix);
    }
    out
}

fn refresh_file_event(change: &crate::app::RefreshedFileChange) -> String {
    // Diff-churn growth from this refresh; shrinking diffs (undo, abandon) just
    // say "updated" rather than negative churn unless the content is recognized
    // as a revert to already-seen content.
    let added = change.additions_delta.max(0);
    let removed = change.deletions_delta.max(0);
    let churn = if added == 0 && removed == 0 {
        String::new()
    } else {
        format!(" (+{added} −{removed})")
    };
    let rereview = if change.was_reviewed {
        " — was viewed, needs re-review"
    } else {
        ""
    };
    if change.reverted_to_seen {
        format!("{} reverted to previously seen content", change.path)
    } else if change.is_new {
        format!("{} appeared{churn}{rereview}", change.path)
    } else {
        format!("{} updated{churn}{rereview}", change.path)
    }
}

fn specialize_description_only_events(events: &mut [String], no_file_events: bool) {
    if !no_file_events {
        return;
    }
    for event in events {
        if let Some(change) = event
            .strip_prefix("change ")
            .and_then(|rest| rest.strip_suffix(" updated"))
        {
            *event = format!("change {change} description updated");
        }
    }
}

fn event_key(message: &str) -> String {
    if let Some(rest) = message.strip_prefix("change ") {
        rest.split_whitespace().next().unwrap_or(message).to_owned()
    } else if message.starts_with("@ moved") {
        "@".to_owned()
    } else {
        message
            .split_whitespace()
            .next()
            .unwrap_or(message)
            .to_owned()
    }
}

fn push_activity(tui_state: &mut TuiState, key: String, message: String) {
    if let Some(last) = tui_state.activity.back_mut()
        && last.key == key
        && last.message == message
    {
        last.count += 1;
        last.timestamp = chrono::Utc::now();
        return;
    }
    tui_state.activity.push_back(ActivityEvent {
        timestamp: chrono::Utc::now(),
        key,
        message,
        count: 1,
    });
}

fn fingerprint_events(previous: &str, current: &str) -> Vec<String> {
    fn parse(input: &str) -> (Option<(String, String)>, BTreeMap<String, String>) {
        let mut at = None;
        let changes = input
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let first = parts.next()?;
                if first == "@" {
                    let change = parts.next()?.to_owned();
                    let commit = parts.next().unwrap_or(&change).to_owned();
                    at = Some((change, commit));
                    return None;
                }
                let second = parts.next();
                match second {
                    Some(commit) => Some((first.to_owned(), commit.to_owned())),
                    None => Some((first.to_owned(), first.to_owned())),
                }
            })
            .collect();
        (at, changes)
    }
    let (old_at, old) = parse(previous);
    let (new_at, new) = parse(current);
    let mut events = Vec::new();
    if let (Some((old_change, _)), Some((new_change, _))) = (&old_at, &new_at)
        && old_change != new_change
    {
        events.push(format!("@ moved to {new_change}"));
    }
    for (change, commit) in &new {
        match old.get(change) {
            None => events.push(format!("change {change} entered range")),
            Some(old_commit) if old_commit != commit => {
                events.push(format!("change {change} updated"))
            }
            _ => {}
        }
    }
    for change in old.keys() {
        if !new.contains_key(change) {
            events.push(format!("change {change} left range"));
        }
    }
    events
}

/// Re-apply the on-disk agent overlay to the session. Reloads (`replace_diff`)
/// reset overlay-derived ordering and flags; refreshes restore them.
fn reapply_agent_overlay(
    session: &mut ReviewSession,
    _review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) {
    let Some(overlay_path) = tui_state.agent_overlay_path.clone() else {
        return;
    };
    if let Ok(overlay) = crate::agent::AgentOverlay::load_or_default(&overlay_path) {
        let transition = tui_state
            .diff_viewport
            .transition_snapshot(session, current_diff_inner(session, tui_state));
        session.apply_agent_overlay(&overlay);
        tui_state.diff_viewport.finish_transition(
            transition,
            session,
            current_diff_inner(session, tui_state),
        );
    }
    // Our own read is not news; suppress the poll-based reload notice.
    tui_state.overlay_mtime = std::fs::metadata(&overlay_path)
        .and_then(|metadata| metadata.modified())
        .ok();
}

/// Durable-state persistence is generation-gated: every durable TUI mutation
/// seam (viewed marks, comments, session-level attention, walkthrough,
/// action-item, disposition, and lifecycle edits) bumps
/// [`ReviewSession::durable_state_generation`], and autosave skips all work
/// when the generation is unchanged. Serializing session-scale state per
/// keystroke to detect changes is a bug.
fn autosave_state(session: &mut ReviewSession, state_path: &Path, tui_state: &mut TuiState) {
    if let Err(error) = persist_review_state(session, state_path, tui_state) {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Error,
            message: format!("failed to autosave review state: {error}"),
        });
    }
}

fn persist_review_state(
    session: &mut ReviewSession,
    state_path: &Path,
    tui_state: &mut TuiState,
) -> Result<()> {
    // Generation gate: when no durable mutation seam fired since the last
    // persist, skip without serializing, fingerprinting, or touching the
    // filesystem. External writers are handled by the idle-tick reload path
    // (`maybe_reload_review_state`), which is mtime-gated.
    if tui_state.last_autosave_generation == Some(session.durable_state_generation()) {
        return Ok(());
    }
    let transition = tui_state
        .diff_viewport
        .transition_snapshot(session, current_diff_inner(session, tui_state));
    let local = session.to_state();
    let fallback = ReviewState::default();
    let base = tui_state.last_persisted_state.as_ref().unwrap_or(&fallback);
    let state =
        crate::review::merge_live_state_file(state_path, base, local, &tui_state.state_tombstones)?;
    session.apply_review_state(state.clone());
    tui_state.diff_viewport.finish_transition(
        transition,
        session,
        current_diff_inner(session, tui_state),
    );
    tui_state.state_mtime = state_file_mtime(state_path);
    tui_state.last_persisted_state = Some(state);
    tui_state.state_tombstones = ReviewStateTombstones::default();
    tui_state.last_autosave_generation = Some(session.durable_state_generation());
    Ok(())
}

#[cfg(unix)]
fn apply_acp_review_mutation(
    mutation: crate::acp::ReviewMutation,
    session: &mut ReviewSession,
    state_path: Option<&Path>,
    tui_state: &mut TuiState,
) -> Result<serde_json::Value> {
    let Some(state_path) = state_path else {
        return Err(color_eyre::eyre::eyre!(
            "durable review state path unavailable"
        ));
    };
    let transition = tui_state
        .diff_viewport
        .transition_snapshot(session, current_diff_inner(session, tui_state));
    let result = persist_acp_review_mutation(
        mutation,
        session,
        state_path,
        tui_state.last_persisted_state.as_ref(),
        &tui_state.state_tombstones,
    )?;
    tui_state.diff_viewport.finish_transition(
        transition,
        session,
        current_diff_inner(session, tui_state),
    );
    tui_state.state_mtime = state_file_mtime(state_path);
    tui_state.last_persisted_state = Some(session.to_state());
    tui_state.state_tombstones = ReviewStateTombstones::default();
    tui_state.last_autosave_generation = Some(session.durable_state_generation());
    Ok(result)
}

#[cfg(unix)]
pub(crate) fn persist_acp_review_mutation(
    mutation: crate::acp::ReviewMutation,
    session: &mut ReviewSession,
    state_path: &Path,
    baseline: Option<&ReviewState>,
    tombstones: &crate::state::ReviewStateTombstones,
) -> Result<serde_json::Value> {
    let before = session.to_state();
    let result = match mutation {
        crate::acp::ReviewMutation::DraftComment { path, line, body } => session
            .add_agent_draft(path, line, body)
            .map(|comment| serde_json::json!({ "id": comment.id }))
            .ok_or_else(|| color_eyre::eyre::eyre!("draft body must contain non-whitespace text")),
    }?;
    let fallback = ReviewState::default();
    let merged = match crate::review::merge_live_state_file(
        state_path,
        baseline.unwrap_or(&fallback),
        session.to_state(),
        tombstones,
    ) {
        Ok(merged) => merged,
        Err(error) => {
            session.apply_review_state(before);
            return Err(error);
        }
    };
    session.apply_review_state(merged);
    Ok(result)
}

fn handle_key_event(
    key: KeyEvent,
    session: &mut ReviewSession,
    mode: &mut Mode,
    keymap: &KeyMap,
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) -> Result<bool> {
    if key.kind != KeyEventKind::Press {
        return Ok(false);
    }

    match mode {
        Mode::Normal => {
            if let Some(action) = keymap.normal_action_for(&key, session.focus == Focus::Diff)
                && handle_normal_action(action, session, mode, review_loader, tui_state)?
            {
                return Ok(true);
            }
        }
        Mode::Help => match keymap.popup_action_for(KeyContext::Help, &key) {
            Some(Action::PopupClose | Action::PopupCloseQ | Action::Help) => *mode = Mode::Normal,
            Some(Action::PopupMoveDown) => {
                tui_state.help_scroll = tui_state.help_scroll.saturating_add(1)
            }
            Some(Action::PopupMoveUp) => {
                tui_state.help_scroll = tui_state.help_scroll.saturating_sub(1)
            }
            _ if key.code == KeyCode::PageDown => {
                tui_state.help_scroll = tui_state.help_scroll.saturating_add(10)
            }
            _ if key.code == KeyCode::PageUp => {
                tui_state.help_scroll = tui_state.help_scroll.saturating_sub(10)
            }
            _ => {}
        },
        Mode::TargetChooser(chooser) => {
            if handle_target_chooser_key(key, chooser, session, keymap, review_loader, tui_state) {
                *mode = Mode::Normal;
            }
        }
        Mode::RevsetInput(input) => {
            if handle_revset_input_key(key, input, session, keymap, review_loader, tui_state) {
                *mode = Mode::Normal;
            }
        }
        Mode::OperationPicker(picker) => {
            if handle_operation_picker_key(key, picker, session, keymap, review_loader, tui_state) {
                *mode = Mode::Normal;
            }
        }
        Mode::JjHelpers(state) => {
            if handle_jj_helpers_key(key, state, session, keymap, review_loader, tui_state) {
                *mode = Mode::Normal;
            }
        }
        Mode::FlagList(list) => {
            if handle_flag_list_key(key, list, session, keymap, tui_state) {
                *mode = Mode::Normal;
            }
        }
        Mode::OpenWork(list) => {
            if handle_open_work_key(key, list, session, keymap, tui_state) {
                *mode = Mode::Normal;
            }
        }
        Mode::Activity(list) => {
            if handle_activity_key(key, list, session, keymap, tui_state) {
                *mode = Mode::Normal;
            }
        }
        Mode::WalkthroughList(list) => {
            if handle_walkthrough_list_key(key, list, session, keymap, tui_state) {
                *mode = Mode::Normal;
            }
        }
        Mode::DraftList(list) => {
            if let Some(next_mode) = handle_draft_list_key(key, list, session, keymap, tui_state) {
                *mode = next_mode;
            }
        }
        Mode::FileSearch(search) => {
            if handle_file_search_key(key, search, session, keymap, tui_state) {
                *mode = Mode::Normal;
            }
        }
        Mode::SymbolOutline(outline) => {
            if handle_symbol_outline_key(key, outline, session, keymap, tui_state) {
                *mode = Mode::Normal;
            }
        }
        Mode::CommentList(list) => {
            let action = keymap.popup_action_for(KeyContext::CommentList, &key);
            if action == Some(Action::CommentListNewGeneral) {
                *mode = Mode::CommentInput {
                    editor: CommentEditor::with_channel(
                        String::new(),
                        inferred_comment_channel(session, tui_state, false, None),
                    ),
                    target: CommentInputTarget::NewGeneral,
                };
            } else if action == Some(Action::EditComment) {
                if let Some(comment) = list
                    .selected_comment_id(session)
                    .and_then(|id| session.comments.iter().find(|comment| comment.id == id))
                {
                    *mode = Mode::CommentInput {
                        editor: CommentEditor::with_channel(comment.body.clone(), comment.channel),
                        target: CommentInputTarget::Edit {
                            id: comment.id.clone(),
                        },
                    };
                }
            } else if handle_comment_list_key(key, list, session, keymap, tui_state) {
                *mode = Mode::Normal;
            }
        }
        Mode::ViewOptions(state) => {
            if handle_view_options_key(key, state, session, keymap, tui_state) {
                *mode = Mode::Normal;
            }
        }
        Mode::AttentionGlance(board) => {
            if handle_attention_glance_key(key, board, session, keymap, tui_state) {
                *mode = Mode::Normal;
            }
        }
        Mode::CommentInput { editor, target } => {
            let mut leave_comment_input = false;
            if let Some(action) = keymap.comment_action_for(&key) {
                leave_comment_input =
                    handle_comment_action(action, session, editor, target, tui_state);
            } else {
                handle_comment_key(key, editor);
            }
            if leave_comment_input {
                *mode = Mode::Normal;
            }
        }
    }
    if mode.review_pointer_policy() == ReviewPointerPolicy::BlockedByModal {
        tui_state.diff_drag = None;
        // A modal opening always closes an open menu dropdown.
        tui_state.menu.close();
    }
    Ok(false)
}

fn handle_normal_action(
    action: Action,
    session: &mut ReviewSession,
    mode: &mut Mode,
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) -> Result<bool> {
    if action == Action::Quit {
        return Ok(true);
    }
    let explicit_viewport_action = matches!(
        action,
        Action::DiffTop
            | Action::DiffBottom
            | Action::NextSymbol
            | Action::PreviousSymbol
            | Action::NextChangedHunk
            | Action::PreviousChangedHunk
            | Action::NextComment
            | Action::PreviousComment
            | Action::ScrollDown
            | Action::ScrollUp
            | Action::ScrollDiffLeft
            | Action::ScrollDiffRight
            | Action::ToggleLargeDiff
    ) || matches!(action, Action::MoveDown | Action::MoveUp)
        && session.focus == Focus::Diff;
    let indirect_viewport_action = matches!(
        action,
        Action::ToggleFocus
            | Action::NextUnviewed
            | Action::PreviousUnviewed
            | Action::NextFile
            | Action::PreviousFile
            | Action::SpotlightNext
            | Action::SpotlightPrevious
            | Action::AdvanceReview
            | Action::AttentionPromote
            | Action::AttentionDemote
            | Action::MarkViewed
            | Action::ToggleViewed
            | Action::MarkAllViewed
            | Action::ToggleGenerated
            | Action::CycleViewedFilter
            | Action::ToggleContextFold
            | Action::ExpandContext
            | Action::ExpandContextAll
            | Action::CollapseContext
            | Action::ToggleDiffWrap
            | Action::ToggleAnnotationArtifacts
            | Action::ToggleFilePane
            | Action::ToggleDiffView
            | Action::CycleCommentState
            | Action::DeleteComment
            | Action::AttentionFocus
    ) || matches!(action, Action::MoveDown | Action::MoveUp)
        && session.focus == Focus::Files;
    let stream_navigation_action = session.stream_mode
        && (matches!(
            action,
            Action::NextUnviewed
                | Action::PreviousUnviewed
                | Action::NextFile
                | Action::PreviousFile
                | Action::MarkViewed
                | Action::SpotlightNext
                | Action::SpotlightPrevious
                | Action::AdvanceReview
                | Action::AttentionPromote
                | Action::AttentionDemote
        ) || matches!(action, Action::MoveDown | Action::MoveUp));
    let transition = (!explicit_viewport_action
        && indirect_viewport_action
        && !stream_navigation_action)
        .then(|| {
            tui_state
                .diff_viewport
                .transition_snapshot(session, current_diff_inner(session, tui_state))
        });
    let top_only_transition = matches!(action, Action::ToggleFocus | Action::ToggleFilePane);
    match action {
        Action::Quit => unreachable!("handled above"),
        Action::Help => {
            tui_state.help_scroll = 0;
            *mode = Mode::Help;
        }
        Action::YankHandoff => yank_handoff(session, tui_state),
        Action::MoveDown => match session.focus {
            Focus::Files => session.move_selection(1),
            Focus::Diff => {
                let had_range = session.has_active_diff_range();
                if if session.stream_mode {
                    session.move_stream_cursor(1)
                } else {
                    session.move_diff_cursor(1)
                } {
                    ensure_diff_cursor_visible(
                        session,
                        current_diff_inner(session, tui_state),
                        tui_state,
                    );
                }
                if had_range && !session.has_active_diff_range() {
                    tui_state.notice = Some(UiNotice {
                        level: UiNoticeLevel::Info,
                        message: "range selection cancelled at file boundary".to_owned(),
                    });
                }
            }
        },
        Action::MoveUp => match session.focus {
            Focus::Files => session.move_selection(-1),
            Focus::Diff => {
                let had_range = session.has_active_diff_range();
                if if session.stream_mode {
                    session.move_stream_cursor(-1)
                } else {
                    session.move_diff_cursor(-1)
                } {
                    ensure_diff_cursor_visible(
                        session,
                        current_diff_inner(session, tui_state),
                        tui_state,
                    );
                }
                if had_range && !session.has_active_diff_range() {
                    tui_state.notice = Some(UiNotice {
                        level: UiNoticeLevel::Info,
                        message: "range selection cancelled at file boundary".to_owned(),
                    });
                }
            }
        },
        Action::ToggleFocus => {
            session.toggle_focus();
            tui_state.correct_file_pane_focus(session, tui_state.terminal_size.width);
        }
        Action::DiffTop => {
            tui_state
                .diff_viewport
                .place_top(session, current_diff_inner(session, tui_state));
        }
        Action::DiffBottom => {
            let inner = current_diff_inner(session, tui_state);
            scroll_diff_to_bottom_visual(session, inner, tui_state);
        }
        Action::CompareTrunk => load_review_target(
            review_loader,
            session,
            tui_state
                .launch_target
                .clone()
                .unwrap_or_else(ReviewTarget::trunk_to_current),
            tui_state,
        ),
        Action::CompareParent => load_review_target(
            review_loader,
            session,
            ReviewTarget::parent_to_current(),
            tui_state,
        ),
        Action::TargetChooser => match review_loader.base_candidates(session) {
            Ok(candidates) if candidates.is_empty() => {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no jj changes found for base picker".to_owned(),
                });
            }
            Ok(candidates) => {
                *mode = Mode::TargetChooser(TargetChooserState::new(
                    candidates,
                    &session.target.base,
                    &session.target.rev,
                ));
            }
            Err(error) => {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Error,
                    message: format!("failed to load jj changes: {error:?}"),
                });
            }
        },
        Action::RevsetInput => {
            *mode = Mode::RevsetInput(RevsetInputState::new(
                &session.target.base,
                &session.target.rev,
            ));
        }
        Action::StackNext => step_stack(review_loader, session, 1, tui_state),
        Action::StackPrevious => step_stack(review_loader, session, -1, tui_state),
        Action::OperationPicker => match review_loader.jj.operations(&session.repo) {
            Ok(operations) if operations.is_empty() => {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no jj operations found".to_owned(),
                });
            }
            Ok(operations) => {
                let mut picker = OperationPickerState::new(operations);
                refresh_operation_picker_preview(&mut picker, session, review_loader);
                *mode = Mode::OperationPicker(picker);
            }
            Err(error) => {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Error,
                    message: format!("failed to load jj operations: {error:?}"),
                });
            }
        },
        Action::JjHelpers => {
            *mode = Mode::JjHelpers(JjHelperState::for_session(session));
        }
        Action::FlagList => {
            if session.agent_flags.is_empty() {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no agent-flagged sections".to_owned(),
                });
            } else {
                *mode = Mode::FlagList(FlagListState::new(session));
            }
        }
        Action::OpenWork => {
            let open_work = OpenWorkListState::new(session);
            if open_work.rows.is_empty() {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no open action items or todo feedback".to_owned(),
                });
            } else {
                *mode = Mode::OpenWork(open_work);
            }
        }
        Action::Activity => {
            *mode = Mode::Activity(ActivityListState::new());
        }
        Action::WalkthroughList => {
            let walkthroughs = WalkthroughListState::new(session);
            if walkthroughs.step_ids.is_empty() {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no walkthrough steps".to_owned(),
                });
            } else {
                *mode = Mode::WalkthroughList(walkthroughs);
            }
        }
        Action::DraftList => {
            let drafts = DraftListState::new(session);
            if drafts.drafts.is_empty() {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no pending agent drafts".to_owned(),
                });
            } else {
                *mode = Mode::DraftList(drafts);
            }
        }
        Action::NextUnviewed => session.move_to_unviewed(1),
        Action::PreviousUnviewed => session.move_to_unviewed(-1),
        Action::NextFile => session.move_file_selection(1),
        Action::PreviousFile => session.move_file_selection(-1),
        Action::SpotlightNext => {
            if let Some((step_id, part)) = session.jump_spotlight_with_identity(1) {
                if tui_state.attention_focus.is_some() {
                    repin_exact_spotlight_narration(session, tui_state, &step_id, part);
                }
                tui_state
                    .diff_viewport
                    .place_cursor(session, current_diff_inner(session, tui_state));
            }
        }
        Action::SpotlightPrevious => {
            if let Some((step_id, part)) = session.jump_spotlight_with_identity(-1) {
                if tui_state.attention_focus.is_some() {
                    repin_exact_spotlight_narration(session, tui_state, &step_id, part);
                }
                tui_state
                    .diff_viewport
                    .place_cursor(session, current_diff_inner(session, tui_state));
            }
        }
        Action::AdvanceReview => {
            if session.stream_mode && session.focus == Focus::Diff {
                let _ = session.acknowledge_selected_skim_fold();
            }
            if let Some((step_id, part)) = session.advance_review_spotlight() {
                if tui_state.attention_focus.is_some() {
                    repin_exact_spotlight_narration(session, tui_state, &step_id, part);
                }
                tui_state
                    .diff_viewport
                    .place_cursor(session, current_diff_inner(session, tui_state));
            } else {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "review tour complete".to_owned(),
                });
            }
        }
        Action::AttentionPromote => {
            let changed = session.change_selected_salience(true);
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: if changed {
                    "promoted region with durable human precedence".to_owned()
                } else {
                    "no current attention region under cursor".to_owned()
                },
            });
        }
        Action::AttentionDemote => {
            let changed = session.change_selected_salience(false);
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: if changed {
                    "demoted region with durable human precedence".to_owned()
                } else {
                    "no current attention region under cursor".to_owned()
                },
            });
        }
        Action::AttentionFocus => {
            let active = tui_state.toggle_attention_focus(session);
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: if active {
                    "Focus applied — pane hidden, context maximally folded, narration pinned"
                        .to_owned()
                } else {
                    "Focus restored the prior review view".to_owned()
                },
            });
        }
        Action::AttentionGlance => {
            let board = GlanceBoardState::new(session);
            if board.rows.is_empty() {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no current or stale skim folds on the attention map".to_owned(),
                });
            } else {
                *mode = Mode::AttentionGlance(board);
            }
        }
        Action::FileSearch => {
            *mode = Mode::FileSearch(FileSearchState::new(session));
        }
        Action::SymbolOutline => {
            let outline = SymbolOutlineState::new(session);
            if outline.targets.is_empty() {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no changed symbols in this file".to_owned(),
                });
            } else {
                *mode = Mode::SymbolOutline(outline);
            }
        }
        Action::NextSymbol => {
            if session.jump_to_changed_symbol(1) {
                ensure_diff_cursor_visible(
                    session,
                    current_diff_inner(session, tui_state),
                    tui_state,
                );
            }
        }
        Action::PreviousSymbol => {
            if session.jump_to_changed_symbol(-1) {
                ensure_diff_cursor_visible(
                    session,
                    current_diff_inner(session, tui_state),
                    tui_state,
                );
            }
        }
        Action::NextChangedHunk => {
            if session.jump_to_changed_hunk(1) {
                ensure_diff_cursor_visible(
                    session,
                    current_diff_inner(session, tui_state),
                    tui_state,
                );
            }
        }
        Action::PreviousChangedHunk => {
            if session.jump_to_changed_hunk(-1) {
                ensure_diff_cursor_visible(
                    session,
                    current_diff_inner(session, tui_state),
                    tui_state,
                );
            }
        }
        Action::CommentList => {
            *mode = Mode::CommentList(CommentListState::default());
        }
        Action::NextComment => {
            if let Some(selection) = session.move_to_comment(1) {
                if let Some(comment) = session.selected_comment() {
                    tui_state.diff_viewport.select_annotation(
                        session,
                        annotation_card::AnnotationSource::Comment {
                            id: comment.id.clone(),
                        },
                    );
                }
                apply_navigation_viewport_placement(
                    session,
                    match selection {
                        CommentSelection::File => NavigationViewportPlacement::Keep,
                        CommentSelection::Diff => NavigationViewportPlacement::Cursor,
                    },
                    tui_state,
                );
            }
        }
        Action::PreviousComment => {
            if let Some(selection) = session.move_to_comment(-1) {
                if let Some(comment) = session.selected_comment() {
                    tui_state.diff_viewport.select_annotation(
                        session,
                        annotation_card::AnnotationSource::Comment {
                            id: comment.id.clone(),
                        },
                    );
                }
                apply_navigation_viewport_placement(
                    session,
                    match selection {
                        CommentSelection::File => NavigationViewportPlacement::Keep,
                        CommentSelection::Diff => NavigationViewportPlacement::Cursor,
                    },
                    tui_state,
                );
            }
        }
        Action::ScrollDown => scroll_diff_visual(
            session,
            current_diff_inner(session, tui_state),
            12,
            tui_state,
        ),
        Action::ScrollUp => scroll_diff_visual(
            session,
            current_diff_inner(session, tui_state),
            -12,
            tui_state,
        ),
        Action::ScrollDiffLeft => scroll_diff_horizontal_visual(
            session,
            current_diff_inner(session, tui_state),
            -4,
            tui_state,
        ),
        Action::ScrollDiffRight => scroll_diff_horizontal_visual(
            session,
            current_diff_inner(session, tui_state),
            4,
            tui_state,
        ),
        Action::MarkViewed => session.mark_selected_viewed(),
        Action::ToggleViewed => session.toggle_viewed(),
        Action::MarkAllViewed => {
            if session.stream_mode && session.focus == Focus::Diff {
                let message = match session.acknowledge_selected_skim_fold() {
                    SkimAcknowledgeResult::Acknowledged => "acknowledged current skim region",
                    SkimAcknowledgeResult::NotFold => {
                        "select a skim fold before acknowledging; no files were marked viewed"
                    }
                    SkimAcknowledgeResult::Unavailable => {
                        "skim acknowledgement unavailable; no files were marked viewed"
                    }
                };
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: message.to_owned(),
                });
            } else {
                session.mark_all_viewed();
            }
        }
        Action::ToggleGenerated => session.toggle_generated_visibility(),
        Action::CycleViewedFilter => session.cycle_viewed_filter(),
        Action::ToggleFold => {
            if session.focus == Focus::Files {
                session.toggle_tree_fold();
            } else if let Some(expanded) = session.toggle_selected_skim_fold() {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: if expanded {
                        "peeked into skim fold".to_owned()
                    } else {
                        "collapsed skim fold".to_owned()
                    },
                });
            }
        }
        Action::CollapseFold => {
            if session.focus == Focus::Files {
                session.collapse_tree_node();
            }
        }
        Action::ExpandFold => {
            if session.focus == Focus::Files {
                session.expand_tree_node();
            }
        }
        Action::ToggleContextFold => {
            session.toggle_context_fold();
        }
        Action::ExpandContext => {
            if session.focus == Focus::Diff {
                let step = session.diff_cues.context_step;
                expand_diff_context(session, review_loader, tui_state, Some(step));
            }
        }
        Action::ExpandContextAll => {
            if session.focus == Focus::Diff {
                expand_diff_context(session, review_loader, tui_state, None);
            }
        }
        Action::CollapseContext => {
            if session.focus == Focus::Diff && !session.collapse_nearest_gap() {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no expanded context to collapse here".to_owned(),
                });
            }
        }
        Action::ViewOptions => *mode = Mode::ViewOptions(ViewOptionsState::default()),
        Action::ToggleWordHighlight => session.toggle_word_highlight(),
        Action::ToggleLineBackground => session.toggle_line_background(),
        Action::ToggleGutterBar => session.toggle_gutter_bar(),
        Action::ToggleDiffWrap => {
            session.toggle_diff_wrap();
        }
        Action::ToggleAnnotationArtifacts => {
            if session.focus == Focus::Diff {
                match tui_state.diff_viewport.toggle_annotation_artifacts(session) {
                    Some(true) => {
                        tui_state.notice = Some(UiNotice {
                            level: UiNoticeLevel::Info,
                            message: "expanded annotation artifacts".to_owned(),
                        });
                    }
                    Some(false) => {
                        tui_state.notice = Some(UiNotice {
                            level: UiNoticeLevel::Info,
                            message: "collapsed annotation artifacts".to_owned(),
                        });
                    }
                    None => {
                        tui_state.notice = Some(UiNotice {
                            level: UiNoticeLevel::Info,
                            message: "no annotation artifacts on this row".to_owned(),
                        });
                    }
                }
            }
        }
        Action::ToggleFilePane => {
            tui_state.toggle_file_pane(session, tui_state.terminal_size.width);
        }
        Action::ToggleDiffView => {
            session.toggle_diff_view();
        }
        Action::WidenFilePane => {
            let transition = tui_state
                .diff_viewport
                .transition_snapshot(session, current_diff_inner(session, tui_state));
            tui_state.file_pane.split_percent =
                tui_state.file_pane.split_percent.saturating_add(5).min(60);
            tui_state.diff_viewport.finish_transition(
                transition,
                session,
                current_diff_inner(session, tui_state),
            );
        }
        Action::NarrowFilePane => {
            let transition = tui_state
                .diff_viewport
                .transition_snapshot(session, current_diff_inner(session, tui_state));
            tui_state.file_pane.split_percent =
                tui_state.file_pane.split_percent.saturating_sub(5).max(10);
            tui_state.diff_viewport.finish_transition(
                transition,
                session,
                current_diff_inner(session, tui_state),
            );
        }
        Action::ToggleLargeDiff => {
            session.toggle_large_diff_render();
            tui_state
                .diff_viewport
                .place_cursor(session, current_diff_inner(session, tui_state));
        }
        Action::ToggleAgentOrder => {
            session.toggle_agent_order();
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: if session.agent_order_active() {
                    "agent-suggested review order enabled".to_owned()
                } else if session.agent_ordering.is_empty() {
                    "no agent ordering suggested yet".to_owned()
                } else {
                    "agent-suggested review order disabled".to_owned()
                },
            });
        }
        Action::RangeComment => {
            if session.focus == Focus::Diff {
                session.toggle_diff_range_selection();
            }
        }
        Action::MarkWalkthrough => {
            if session.focus == Focus::Diff {
                match add_walkthrough_step_from_selection(session) {
                    Some((index, label)) => {
                        tui_state.notice = Some(UiNotice {
                            level: UiNoticeLevel::Info,
                            message: format!("added walkthrough step {index}: {label}"),
                        });
                    }
                    None => {
                        tui_state.notice = Some(UiNotice {
                            level: UiNoticeLevel::Info,
                            message: "no diff line selected for walkthrough".to_owned(),
                        });
                    }
                }
            }
        }
        Action::CancelRangeComment => {
            // Esc-style dismissal: an open menu dropdown is the topmost
            // transient layer, then range selection and the footer notice.
            if !tui_state.menu.close() {
                session.clear_diff_range_selection();
                tui_state.notice = None;
            }
        }
        Action::Comment => {
            let onboarding_target = selected_onboarding_target(session, tui_state);
            *mode = Mode::CommentInput {
                editor: CommentEditor::with_channel(
                    String::new(),
                    inferred_comment_channel(session, tui_state, onboarding_target, None),
                ),
                target: CommentInputTarget::New,
            };
        }
        Action::CycleCommentState => {
            if let Some(id) = session.selected_comment().map(|comment| comment.id.clone()) {
                if let Some(state) = session.cycle_comment_state(&id) {
                    tui_state.notice = Some(UiNotice {
                        level: UiNoticeLevel::Info,
                        message: format!("comment state: {}", state.label()),
                    });
                }
            } else {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no comment selected to update".to_owned(),
                });
            }
        }
        Action::EditComment => {
            if let Some(comment) = session.selected_comment() {
                *mode = Mode::CommentInput {
                    editor: CommentEditor::with_channel(comment.body.clone(), comment.channel),
                    target: CommentInputTarget::Edit {
                        id: comment.id.clone(),
                    },
                };
            } else {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no comment selected to edit".to_owned(),
                });
            }
        }
        Action::DeleteComment => {
            if let Some(id) = session.selected_comment().map(|comment| comment.id.clone()) {
                session.delete_comment(&id);
                tui_state.state_tombstones.comments.insert(id);
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "deleted comment".to_owned(),
                });
            } else {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no comment selected to delete".to_owned(),
                });
            }
        }
        Action::SubmitComment
        | Action::CancelComment
        | Action::InsertNewline
        | Action::DeleteChar
        | Action::CycleCommentChannel
        | Action::TargetPickerMoveDown
        | Action::TargetPickerMoveUp
        | Action::PopupMoveDown
        | Action::PopupMoveUp
        | Action::PopupSelect
        | Action::PopupToggle
        | Action::PopupClose
        | Action::PopupCloseQ
        | Action::CommentListNewGeneral
        | Action::CommentListReady
        | Action::CommentListCycleIntent
        | Action::CommentListCycleKind
        | Action::DraftAccept
        | Action::DraftEdit
        | Action::DraftDiscard
        | Action::WalkthroughDelete
        | Action::WalkthroughMoveDown
        | Action::WalkthroughMoveUp
        | Action::GlancePeek
        | Action::GlanceAcknowledge
        | Action::GlanceAcknowledgeAll => {}
    }
    // Only indirect row/file/geometry mutations need a snapshot pair.
    // Explicit scrolling and placement already cross the controller boundary,
    // while popup/help/style actions must not rescan annotations on every key.
    if let Some(transition) = transition {
        if top_only_transition {
            tui_state.diff_viewport.finish_transition_top_only(
                transition,
                session,
                current_diff_inner(session, tui_state),
            );
        } else {
            tui_state.diff_viewport.finish_transition(
                transition,
                session,
                current_diff_inner(session, tui_state),
            );
        }
    }
    Ok(false)
}

fn repin_exact_spotlight_narration(
    session: &mut ReviewSession,
    tui_state: &TuiState,
    step_id: &str,
    part: usize,
) {
    let target = session
        .active_durable_session()
        .into_iter()
        .flat_map(|durable| durable.walkthroughs.iter())
        .flat_map(|walkthrough| walkthrough.steps.iter())
        .find(|step| step.id == step_id)
        .and_then(|step| {
            std::iter::once(&step.target)
                .chain(step.extra_targets.iter())
                .nth(part)
                .cloned()
        });
    if let Some(owner) = target
        .as_ref()
        .and_then(|target| session.stream_walkthrough_card_owner(target))
    {
        session.select_stream_row(owner, true);
    }
    tui_state.diff_viewport.select_annotation(
        session,
        annotation_card::AnnotationSource::Walkthrough {
            step_id: step_id.to_owned(),
            part,
        },
    );
}

fn current_diff_inner(session: &ReviewSession, tui_state: &TuiState) -> Rect {
    let size = tui_state.terminal_size;
    inner_bordered(
        tui_state
            .review_layout(session, Rect::new(0, 0, size.width, size.height))
            .diff,
    )
}

fn transition_to_logical_selection(session: &mut ReviewSession, tui_state: &TuiState) {
    tui_state.diff_viewport.file_restored(session);
    tui_state
        .diff_viewport
        .place_cursor(session, current_diff_inner(session, tui_state));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NavigationViewportPlacement {
    Keep,
    Top,
    Cursor,
}

fn apply_navigation_viewport_placement(
    session: &mut ReviewSession,
    placement: NavigationViewportPlacement,
    tui_state: &mut TuiState,
) {
    let inner = current_diff_inner(session, tui_state);
    match placement {
        NavigationViewportPlacement::Keep => {
            tui_state.diff_viewport.file_restored(session);
            tui_state.diff_viewport.reflow(session, inner, false);
        }
        NavigationViewportPlacement::Top => tui_state.diff_viewport.place_top(session, inner),
        NavigationViewportPlacement::Cursor => {
            tui_state.diff_viewport.file_restored(session);
            tui_state.diff_viewport.place_cursor(session, inner);
        }
    }
}

fn core_navigation_placement(placement: NavigationPlacement) -> NavigationViewportPlacement {
    match placement {
        NavigationPlacement::Keep => NavigationViewportPlacement::Keep,
        NavigationPlacement::Top => NavigationViewportPlacement::Top,
        NavigationPlacement::Cursor => NavigationViewportPlacement::Cursor,
    }
}

fn ensure_tui_review_session(session: &mut ReviewSession) -> &mut crate::state::ReviewSession {
    session.ensure_active_durable_session_mut()
}

fn walkthrough_target_from_selection(
    session: &ReviewSession,
) -> Option<crate::state::ReviewTarget> {
    let anchor = session
        .selected_range_anchor()
        .or_else(|| session.selected_line_anchor())?;
    let line = anchor.line()?;
    Some(crate::state::ReviewTarget {
        repo: Some(session.canonical_repo().to_owned()),
        base: Some(session.target.base.clone()),
        revision: Some(session.target.rev.clone()),
        file: Some(anchor.path().to_owned()),
        line: Some(line),
        end_line: anchor.end_line().filter(|end| *end != line),
        anchor: Some(anchor),
        ..Default::default()
    })
}

fn walkthrough_step_title(_session: &ReviewSession, target: &crate::state::ReviewTarget) -> String {
    let file = target.file.as_deref().unwrap_or("<unknown>");
    match (target.line, target.end_line) {
        (Some(line), Some(end)) => format!("{file}:{line}-{end}"),
        (Some(line), None) => format!("{file}:{line}"),
        _ => file.to_owned(),
    }
}

fn add_walkthrough_step_from_selection(session: &mut ReviewSession) -> Option<(usize, String)> {
    let target = walkthrough_target_from_selection(session)?;
    let title = walkthrough_step_title(session, &target);
    let author = session.human_identity.clone();
    let durable = ensure_tui_review_session(session);
    let step = review::add_walkthrough_step(
        durable,
        WalkthroughStep {
            target,
            author: Some(author),
            title: Some(title.clone()),
            ..Default::default()
        },
    );
    let _ = crate::attention::set_human_attention(
        durable,
        step.target.clone(),
        crate::state::Salience::Spotlight,
        step.why.clone(),
    );
    let index = durable
        .walkthroughs
        .first()
        .and_then(|w| w.steps.iter().position(|s| s.id == step.id))
        .map(|i| i + 1)
        .unwrap_or(1);
    Some((index, title))
}

/// Expand hidden hunk context near the diff cursor (docs/focused-diff-ux.md
/// §5). Fetches the full file contents lazily via the jj backend on first
/// use — the new-side revision suffices because context lines are identical
/// on both sides. `step` is the number of lines to reveal (`None` expands
/// the gap fully).
fn expand_diff_context(
    session: &mut ReviewSession,
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
    step: Option<usize>,
) {
    let Some(path) = session
        .selected_visible_file()
        .map(|file| file.path.clone())
    else {
        return;
    };
    if !session.has_file_contents_entry(&path) {
        match review_loader
            .jj
            .file_contents(&session.repo, &session.target.rev, &path)
        {
            Ok(contents) => session.store_file_contents(&path, Some(contents)),
            Err(error) => {
                // Remember the failure so gap rows stop offering expansion.
                session.store_file_contents(&path, None);
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: format!("context expansion unavailable: {error}"),
                });
                return;
            }
        }
    }
    if !session.file_contents_loaded(&path) {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Info,
            message: "context expansion unavailable for this file".to_owned(),
        });
        return;
    }
    if !session.expand_nearest_gap(step) {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Info,
            message: "no hidden context to expand here".to_owned(),
        });
    }
}

/// Step through the current stack (`trunk()..@`) change-by-change, reviewing
/// each change against its parent.
fn step_stack(
    review_loader: &ReviewLoader<'_>,
    session: &mut ReviewSession,
    delta: isize,
    tui_state: &mut TuiState,
) {
    let stack_target = tui_state.launch_target.as_ref().unwrap_or(&session.target);
    let mut stack = match review_loader.jj.stack_changes(&session.repo, stack_target) {
        Ok(stack) => stack,
        Err(error) => {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: format!("failed to load stack: {error:?}"),
            });
            return;
        }
    };
    stack.retain(|change| !change.matches_rev(&stack_target.base));
    let mut dropped_empty_at = false;
    if stack_target.rev == "@"
        && let Some(change) = stack.last()
        && change.description.trim().is_empty()
        && review_loader
            .jj
            .diff(
                &session.repo,
                &ReviewTarget::new(format!("{}-", change.change_id), change.change_id.clone()),
            )
            .is_ok_and(|diff| diff.trim().is_empty())
    {
        stack.pop();
        dropped_empty_at = true;
    }
    if stack.is_empty() {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Info,
            message: "no stack changes between trunk() and @".to_owned(),
        });
        return;
    }

    let current = stack
        .iter()
        .position(|change| change.matches_rev(&session.target.rev))
        .or_else(|| (session.target.rev == "@" && !dropped_empty_at).then(|| stack.len() - 1));
    let next = match current {
        Some(index) => {
            let max = stack.len() as isize - 1;
            (index as isize + delta).clamp(0, max) as usize
        }
        None if delta.is_negative() => stack.len() - 1,
        None => 0,
    };
    if current == Some(next) {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Info,
            message: format!(
                "already at the {} of the stack",
                if delta.is_negative() { "bottom" } else { "top" }
            ),
        });
        return;
    }

    let change = &stack[next];
    let target = ReviewTarget::new(format!("{}-", change.change_id), change.change_id.clone());
    let attention_focus = tui_state.suspend_attention_focus_for_load(session);
    match review_loader.load(session, target) {
        Ok(()) => {
            tui_state.diff_viewport.reset(session);
            if let Some(state) = attention_focus.clone() {
                tui_state.resume_attention_focus_after_load(session, state);
            }
            reconcile_present_spotlight(session, tui_state);
            let description = if change.description.is_empty() {
                "(no description)"
            } else {
                change.title()
            };
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: format!("stack {}/{}: {description}", next + 1, stack.len()),
            });
        }
        Err(error) => {
            if let Some(state) = attention_focus {
                tui_state.resume_attention_focus_after_load(session, state);
            }
            reconcile_present_spotlight(session, tui_state);
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: format!("failed to load stack change: {error:?}"),
            });
        }
    }
}

fn load_change_diffs_for_stack(
    review_loader: &ReviewLoader<'_>,
    session: &mut ReviewSession,
    stack: &[JjChangeSummary],
) {
    session.stack_changes = stack.to_vec();
    for change in stack {
        if session
            .change_diffs
            .iter()
            .any(|(id, _)| change_ids_match_for_tui(id, &change.change_id))
        {
            continue;
        }
        let target = ReviewTarget::new(format!("{}-", change.change_id), change.change_id.clone());
        if let Ok(raw) = review_loader.jj.diff(&session.repo, &target)
            && let Ok(diff) = DiffSet::parse(&raw)
        {
            session.change_diffs.push((change.change_id.clone(), diff));
        }
    }
    // Stack chapters and their per-change diffs feed the stream projection;
    // they are not observable through the cheap cache key, so bump the
    // generation at this mutation seam.
    session.touch_stream_inputs();
}

fn reload_stream_chapter_metadata(review_loader: &ReviewLoader<'_>, session: &mut ReviewSession) {
    let mut stack = review_loader
        .jj
        .stack_changes(&session.repo, &session.target)
        .unwrap_or_default();
    stack.retain(|change| !change.matches_rev(&session.target.base));
    // A live refresh can update the working-copy diff without changing its
    // change id. Reload chapter diffs rather than retaining same-id stale
    // stats; all jj calls remain read-only through the backend.
    session.change_diffs.clear();
    load_change_diffs_for_stack(review_loader, session, &stack);
}

fn change_ids_match_for_tui(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && (a.starts_with(b) || b.starts_with(a))
}

fn handle_target_chooser_key(
    key: KeyEvent,
    chooser: &mut TargetChooserState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) -> bool {
    if let Some(action) = keymap.filter_action_for(KeyContext::TargetChooser, &key) {
        match action {
            Action::TargetPickerMoveDown => chooser.move_selection(1),
            Action::TargetPickerMoveUp => chooser.move_selection(-1),
            Action::PopupClose => return true,
            Action::PopupSelect => {
                if let Some(target) = chooser.target() {
                    load_review_target(review_loader, session, target, tui_state);
                }
                return true;
            }
            _ => {}
        }
        return false;
    }

    match key.code {
        KeyCode::Home => {
            chooser.select_first();
            false
        }
        KeyCode::End => {
            chooser.select_last();
            false
        }
        KeyCode::Tab => {
            chooser.toggle_side();
            false
        }
        KeyCode::Backspace => {
            chooser.pop_query_char();
            false
        }
        KeyCode::Char(ch) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
            chooser.push_query_char(ch);
            false
        }
        _ => false,
    }
}

fn handle_revset_input_key(
    key: KeyEvent,
    input: &mut RevsetInputState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) -> bool {
    match keymap.popup_action_for(KeyContext::RevsetInput, &key) {
        Some(Action::PopupClose) => return true,
        Some(Action::PopupSelect) => {
            return match input.target() {
                Some(target) => {
                    load_review_target(review_loader, session, target, tui_state);
                    true
                }
                None => {
                    tui_state.notice = Some(UiNotice {
                        level: UiNoticeLevel::Info,
                        message: "both base and tip revsets are required".to_owned(),
                    });
                    false
                }
            };
        }
        _ => {}
    }
    match key.code {
        KeyCode::Tab | KeyCode::Up | KeyCode::Down => {
            input.toggle_field();
            false
        }
        KeyCode::Backspace => {
            input.pop_char();
            false
        }
        KeyCode::Char(ch) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
            input.push_char(ch);
            false
        }
        _ => false,
    }
}

fn handle_operation_picker_key(
    key: KeyEvent,
    picker: &mut OperationPickerState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) -> bool {
    if let Some(action) = keymap.popup_action_for(KeyContext::OperationPicker, &key) {
        match action {
            Action::PopupMoveDown => {
                picker.move_selection(1);
                refresh_operation_picker_preview(picker, session, review_loader);
                return false;
            }
            Action::PopupMoveUp => {
                picker.move_selection(-1);
                refresh_operation_picker_preview(picker, session, review_loader);
                return false;
            }
            Action::PopupSelect => {
                if let Some(operation) = picker.selected_operation().cloned() {
                    apply_incremental_review(review_loader, session, &operation, tui_state);
                }
                return true;
            }
            Action::PopupClose => return true,
            _ => {}
        }
    }
    false
}

fn refresh_operation_picker_preview(
    picker: &mut OperationPickerState,
    session: &ReviewSession,
    review_loader: &ReviewLoader<'_>,
) {
    let preview = picker
        .selected_operation()
        .map(|operation| match load_prior_fingerprints(review_loader, session, operation) {
            Ok(prior_fingerprints) => {
                let (caught_up, already_viewed, changed) =
                    preview_incremental_review(session, &prior_fingerprints);
                format!(
                    "will mark {caught_up} caught up · {already_viewed} already viewed · {changed} need re-review"
                )
            }
            Err(error) => format!(
                "preview unavailable for {}: {error:?}",
                operation.operation_id
            ),
        });
    picker.set_preview(preview);
}

fn load_prior_fingerprints(
    review_loader: &ReviewLoader<'_>,
    session: &ReviewSession,
    operation: &crate::jj::JjOperationSummary,
) -> Result<BTreeMap<String, String>> {
    let prior = review_loader
        .jj
        .diff_at_operation(&session.repo, &session.target, &operation.operation_id)
        .and_then(|diff_text| DiffSet::parse(&diff_text))?;
    Ok(prior
        .files
        .into_iter()
        .map(|file| (file.path, file.fingerprint))
        .collect())
}

fn preview_incremental_review(
    session: &ReviewSession,
    prior_fingerprints: &BTreeMap<String, String>,
) -> (usize, usize, usize) {
    let mut preview = session.clone();
    preview.apply_incremental_review(prior_fingerprints)
}

/// Compare the current diff against the same target at a prior operation:
/// unchanged files are marked viewed, changed/new files marked unviewed.
fn apply_incremental_review(
    review_loader: &ReviewLoader<'_>,
    session: &mut ReviewSession,
    operation: &crate::jj::JjOperationSummary,
    tui_state: &mut TuiState,
) {
    let attention_focus = tui_state.suspend_attention_focus_for_load(session);
    let target = session.target.clone();
    let viewport_snapshot = tui_state
        .diff_viewport
        .refresh_snapshot(session, current_diff_inner(session, tui_state));
    if let Err(error) = review_loader.load_in_place(session, target.clone()) {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Error,
            message: format!(
                "failed to refresh {target} before prior-operation compare: {error:?}"
            ),
        });
        if let Some(state) = attention_focus.clone() {
            tui_state.resume_attention_focus_after_load(session, state);
        }
        return;
    }
    tui_state.diff_viewport.refreshed(
        viewport_snapshot,
        session,
        current_diff_inner(session, tui_state),
    );
    let prior_fingerprints = match load_prior_fingerprints(review_loader, session, operation) {
        Ok(prior_fingerprints) => prior_fingerprints,
        Err(error) => {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: format!(
                    "failed to load diff at operation {}: {error:?}",
                    operation.operation_id
                ),
            });
            if let Some(state) = attention_focus.clone() {
                tui_state.resume_attention_focus_after_load(session, state);
            }
            return;
        }
    };
    let transition = tui_state
        .diff_viewport
        .transition_snapshot(session, current_diff_inner(session, tui_state));
    let (caught_up, already_viewed, changed) =
        session.apply_incremental_review(&prior_fingerprints);
    tui_state.diff_viewport.finish_transition(
        transition,
        session,
        current_diff_inner(session, tui_state),
    );
    tui_state.notice = Some(UiNotice {
        level: UiNoticeLevel::Info,
        message: format!(
            "{caught_up} file(s) caught up (unchanged since {}), {already_viewed} already viewed; {changed} changed/new file(s) need re-review",
            operation.operation_id
        ),
    });
    if let Some(state) = attention_focus {
        tui_state.resume_attention_focus_after_load(session, state);
    }
}

fn handle_jj_helpers_key(
    key: KeyEvent,
    state: &mut JjHelperState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) -> bool {
    if let Some(action) = keymap.popup_action_for(KeyContext::JjHelpers, &key) {
        match action {
            Action::PopupMoveDown => state.move_selection(1),
            Action::PopupMoveUp => state.move_selection(-1),
            Action::PopupClose => {
                if state.confirming {
                    state.confirming = false;
                } else {
                    return true;
                }
            }
            Action::PopupSelect => {
                if state.selected_option().is_none() {
                    return true;
                }
                if !state.confirming {
                    state.confirming = true;
                } else if is_plain_enter(&key) {
                    let option = state.selected_option().cloned().expect("checked above");
                    run_jj_helper(review_loader, session, &option, tui_state);
                    return true;
                }
                // The final verbatim-command confirmation accepts only the
                // immutable Enter key: an OSC 11 reply payload can never
                // contain Enter, so no leaked terminal byte can reach this
                // shell-out under any keybinding configuration
                // (docs/theme.md). Custom `popup-select` bindings still
                // navigate and open the confirm step; they are inert here.
            }
            _ => {}
        }
        return false;
    }
    false
}

/// A literal, unmodified Enter press. The jj helper confirmation is gated on
/// this exact key regardless of `popup-select` configuration.
fn is_plain_enter(key: &KeyEvent) -> bool {
    key.code == KeyCode::Enter && key.modifiers.is_empty()
}

/// Run a confirmed jj helper command and reload the current target so the
/// review reflects the rewritten change.
fn run_jj_helper(
    review_loader: &ReviewLoader<'_>,
    session: &mut ReviewSession,
    option: &helpers::JjHelperOption,
    tui_state: &mut TuiState,
) {
    match review_loader.jj.run_command(&session.repo, &option.args) {
        Ok(_) => {
            let attention_focus = tui_state.suspend_attention_focus_for_load(session);
            let reload = review_loader.load(session, session.target.clone());
            if reload.is_ok() {
                tui_state.diff_viewport.reset(session);
            }
            if let Some(state) = attention_focus {
                tui_state.resume_attention_focus_after_load(session, state);
            }
            tui_state.notice = Some(match reload {
                Ok(()) => UiNotice {
                    level: UiNoticeLevel::Info,
                    message: format!("ran {}", option.command_line()),
                },
                Err(error) => UiNotice {
                    level: UiNoticeLevel::Error,
                    message: format!(
                        "ran {} but failed to reload diff: {error:?}",
                        option.command_line()
                    ),
                },
            });
        }
        Err(error) => {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: format!("{} failed: {error:?}", option.command_line()),
            });
        }
    }
}

/// Keys for the view options popup. Returns `true` when the popup closes.
fn handle_view_options_key(
    key: KeyEvent,
    state: &mut ViewOptionsState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    tui_state: &mut TuiState,
) -> bool {
    // The popup's own binding closes it, so `V` toggles the popup.
    if keymap.normal_action_for(&key, true) == Some(Action::ViewOptions) {
        return true;
    }
    match keymap.popup_action_for(KeyContext::ViewOptions, &key) {
        Some(Action::PopupClose | Action::PopupCloseQ) => true,
        Some(Action::PopupSelect | Action::PopupToggle) => {
            let option = state.selected_option();
            if matches!(
                option,
                ViewOption::WordHighlight | ViewOption::LineBackground | ViewOption::GutterBar
            ) {
                option.toggle(session);
                return false;
            }
            let transition = tui_state
                .diff_viewport
                .transition_snapshot(session, current_diff_inner(session, tui_state));
            if option == ViewOption::FilePane {
                tui_state.toggle_file_pane(session, tui_state.terminal_size.width);
            } else {
                option.toggle(session);
            }
            tui_state.diff_viewport.finish_transition_top_only(
                transition,
                session,
                current_diff_inner(session, tui_state),
            );
            false
        }
        Some(Action::PopupMoveDown) => {
            state.move_selection(1);
            false
        }
        Some(Action::PopupMoveUp) => {
            state.move_selection(-1);
            false
        }
        _ => false,
    }
}

fn handle_flag_list_key(
    key: KeyEvent,
    list: &mut FlagListState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    tui_state: &mut TuiState,
) -> bool {
    if let Some(action) = keymap.popup_action_for(KeyContext::FlagList, &key) {
        match action {
            Action::PopupMoveDown => list.move_selection(1),
            Action::PopupMoveUp => list.move_selection(-1),
            Action::PopupClose => return true,
            Action::PopupSelect => {
                if let Some(flag) = list.selected_flag().cloned()
                    && let Some(placement) = session.jump_to_flag(&flag)
                {
                    apply_navigation_viewport_placement(
                        session,
                        core_navigation_placement(placement),
                        tui_state,
                    );
                }
                return true;
            }
            _ => {}
        }
        return false;
    }
    false
}

/// Normal-review glance popup controls. Enter jumps to the selected fold,
/// Space peeks it in place and closes the popup, while acknowledgements stay
/// on the board so their state change is immediately visible.
fn handle_attention_glance_key(
    key: KeyEvent,
    board: &mut GlanceBoardState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    tui_state: &mut TuiState,
) -> bool {
    match keymap.popup_action_for(KeyContext::AttentionGlance, &key) {
        Some(Action::PopupClose) => true,
        Some(Action::PopupMoveDown) => {
            board.move_selection(1);
            false
        }
        Some(Action::PopupMoveUp) => {
            board.move_selection(-1);
            false
        }
        Some(Action::PopupSelect) => {
            let Some(row) = board.selected().cloned() else {
                return false;
            };
            if !row.current || row.stale {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "stale skim history has no current stream target".to_owned(),
                });
                return false;
            }
            jump_to_glance_fold(session, &row.id, false, tui_state)
        }
        Some(Action::GlancePeek) => {
            let Some(row) = board.selected().cloned() else {
                return false;
            };
            if !row.current || row.stale {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "stale skim history cannot be peeked".to_owned(),
                });
                return false;
            }
            jump_to_glance_fold(session, &row.id, true, tui_state)
        }
        Some(Action::GlanceAcknowledge) => {
            let Some(row) = board.selected().cloned() else {
                return false;
            };
            let outcome = session.acknowledge_skim_fold_id(&row.id);
            board.refresh(session);
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: if outcome.stale > 0 {
                    "stale skim history was not acknowledged".to_owned()
                } else if outcome.acknowledged > 0 {
                    format!(
                        "acknowledged {} skim fold; {} whole file(s) marked viewed",
                        outcome.acknowledged,
                        outcome.whole_files_viewed.len()
                    )
                } else if outcome.already_acknowledged > 0 {
                    "selected skim fold is already acknowledged".to_owned()
                } else {
                    "selected skim fold is no longer current".to_owned()
                },
            });
            false
        }
        Some(Action::GlanceAcknowledgeAll) => {
            let outcome = session.acknowledge_all_current_skims();
            board.refresh(session);
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: format!(
                    "acknowledged {} current skim fold(s); {} whole file(s) marked viewed",
                    outcome.acknowledged,
                    outcome.whole_files_viewed.len()
                ),
            });
            false
        }
        _ => false,
    }
}

fn jump_to_glance_fold(
    session: &mut ReviewSession,
    id: &str,
    peek: bool,
    tui_state: &mut TuiState,
) -> bool {
    let destination = {
        let stream = session.review_stream();
        stream
            .rows
            .iter()
            .enumerate()
            .find(|(_, row)| row.id == id)
            .map(|(index, row)| {
                let expanded = matches!(
                    &row.kind,
                    crate::app::StreamRowKind::SkimFold(fold) if fold.expanded
                );
                (index, expanded)
            })
    };
    let Some((index, already_expanded)) = destination else {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Info,
            message: "selected skim fold is no longer current".to_owned(),
        });
        return false;
    };
    session.stream_mode = true;
    session.select_stream_row(index, false);
    if peek && !already_expanded {
        let _ = session.toggle_selected_skim_fold();
    }
    tui_state
        .diff_viewport
        .place_cursor(session, current_diff_inner(session, tui_state));
    tui_state.notice = Some(UiNotice {
        level: UiNoticeLevel::Info,
        message: if peek {
            "peeked selected skim fold in the review stream".to_owned()
        } else {
            "jumped to selected skim fold".to_owned()
        },
    });
    true
}

/// Returns the next mode when the popup should change state.
fn handle_draft_list_key(
    key: KeyEvent,
    list: &mut DraftListState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    tui_state: &mut TuiState,
) -> Option<Mode> {
    match keymap.popup_action_for(KeyContext::DraftList, &key) {
        Some(Action::PopupClose) => Some(Mode::Normal),
        Some(Action::DraftAccept) => {
            let draft = list.selected_draft().cloned()?;
            if accept_agent_draft(session, tui_state, &draft.id, None) && list.remove(&draft.id) {
                return Some(Mode::Normal);
            }
            None
        }
        Some(Action::DraftEdit) => {
            let draft = list.selected_draft().cloned()?;
            Some(Mode::CommentInput {
                editor: CommentEditor::with_channel(
                    draft.body,
                    inferred_comment_channel(session, tui_state, true, None),
                ),
                target: CommentInputTarget::AcceptDraft { id: draft.id },
            })
        }
        Some(Action::DraftDiscard) => {
            let draft = list.selected_draft().cloned()?;
            session.discard_agent_draft(&draft.id);
            tui_state.state_tombstones.comments.insert(draft.id.clone());
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: "discarded agent draft".to_owned(),
            });
            if list.remove(&draft.id) {
                return Some(Mode::Normal);
            }
            None
        }
        Some(Action::PopupMoveDown) => {
            list.move_selection(1);
            None
        }
        Some(Action::PopupMoveUp) => {
            list.move_selection(-1);
            None
        }
        _ => None,
    }
}

fn selected_onboarding_target(session: &ReviewSession, tui_state: &TuiState) -> bool {
    selected_onboarding_comment_id(session, tui_state).is_some()
        || selected_agent_walkthrough_target(session, tui_state)
}

fn selected_onboarding_comment_id(session: &ReviewSession, tui_state: &TuiState) -> Option<String> {
    if let Some(source) = tui_state.diff_viewport.selected_annotation_source(session) {
        if let Some(comment_id) = source.comment_id() {
            return session
                .comments
                .iter()
                .find(|comment| {
                    comment.id == comment_id
                        && comment.author.kind == AuthorKind::Agent
                        && comment.channel == Channel::Onboarding
                })
                .map(|comment| comment.id.clone());
        }
        return None;
    }
    session
        .selected_comment()
        .filter(|comment| {
            comment.author.kind == AuthorKind::Agent
                && comment.channel == Channel::Onboarding
                && session.selected_comment_card_owner(comment) == Some(session.diff_cursor)
        })
        .map(|comment| comment.id.clone())
}

fn selected_agent_walkthrough_target(session: &ReviewSession, tui_state: &TuiState) -> bool {
    if let Some(source) = tui_state.diff_viewport.selected_annotation_source(session) {
        if let Some(step_id) = source.walkthrough_step_id() {
            return session
                .active_durable_session()
                .into_iter()
                .flat_map(|durable| durable.walkthroughs.iter())
                .flat_map(|walkthrough| walkthrough.steps.iter())
                .find(|step| step.id == step_id)
                .and_then(|step| step.author.as_ref())
                .is_some_and(|author| author.kind == AuthorKind::Agent);
        }
        return false;
    }
    false
}

fn inferred_comment_channel(
    session: &ReviewSession,
    tui_state: &TuiState,
    onboarding_target: bool,
    thread_channel: Option<Channel>,
) -> Channel {
    let active_session_id = session
        .active_durable_session()
        .map(|durable| durable.id.as_str());
    let has_agent_annotation = session.comments.iter().any(|comment| {
        comment.author.kind == AuthorKind::Agent
            && active_session_id.map_or(comment.session_id.is_none(), |session_id| {
                comment.belongs_to_session(session_id)
            })
    });
    let agent_attached = tui_state.agent_contacted || has_agent_annotation;
    review::infer_comment_channel(review::ChannelInferenceContext {
        thread_channel,
        onboarding_target,
        agent_attached,
        configured_human_name: session.configured_human_name.as_deref(),
        configured_human_email: session.configured_human_email.as_deref(),
        target_author_name: session.target_author_name.as_deref(),
        target_author_email: session.target_author_email.as_deref(),
        fixed_default: session.comment_default_channel,
    })
}

/// Accept a pending durable agent draft, optionally with an edited body.
fn accept_agent_draft(
    session: &mut ReviewSession,
    tui_state: &mut TuiState,
    draft_id: &str,
    body_override: Option<String>,
) -> bool {
    let channel = inferred_comment_channel(session, tui_state, true, None);
    accept_agent_draft_with_channel(session, tui_state, draft_id, body_override, channel)
}

fn accept_agent_draft_with_channel(
    session: &mut ReviewSession,
    tui_state: &mut TuiState,
    draft_id: &str,
    body_override: Option<String>,
    channel: Channel,
) -> bool {
    let Some(draft) = session
        .pending_agent_drafts()
        .into_iter()
        .find(|draft| draft.id == draft_id)
    else {
        return false;
    };
    let body = body_override.unwrap_or_else(|| draft.body.clone());
    let transition = tui_state
        .diff_viewport
        .transition_snapshot(session, current_diff_inner(session, tui_state));
    match session.accept_agent_draft(&draft, body, channel) {
        Some(_comment_id) => {
            tui_state.diff_viewport.finish_transition(
                transition,
                session,
                current_diff_inner(session, tui_state),
            );
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: "accepted agent draft as comment".to_owned(),
            });
            true
        }
        None => {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: format!(
                    "cannot accept draft: {} is not in this diff",
                    draft.path.as_deref().unwrap_or("<general>")
                ),
            });
            false
        }
    }
}

fn handle_file_search_key(
    key: KeyEvent,
    search: &mut FileSearchState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    tui_state: &mut TuiState,
) -> bool {
    if let Some(action) = keymap.filter_action_for(KeyContext::FileSearch, &key) {
        match action {
            Action::TargetPickerMoveDown => search.move_selection(1),
            Action::TargetPickerMoveUp => search.move_selection(-1),
            Action::PopupClose => return true,
            Action::PopupSelect => {
                if let Some(file_index) = search.selected_file_index() {
                    let transition = tui_state
                        .diff_viewport
                        .transition_snapshot(session, current_diff_inner(session, tui_state));
                    session.jump_to_file(file_index);
                    tui_state.diff_viewport.finish_transition(
                        transition,
                        session,
                        current_diff_inner(session, tui_state),
                    );
                }
                return true;
            }
            _ => {}
        }
        return false;
    }

    match key.code {
        KeyCode::Backspace => {
            search.pop_query_char();
            false
        }
        KeyCode::Char(ch) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
            search.push_query_char(ch);
            false
        }
        _ => false,
    }
}

fn handle_symbol_outline_key(
    key: KeyEvent,
    outline: &mut SymbolOutlineState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    tui_state: &TuiState,
) -> bool {
    if let Some(action) = keymap.popup_action_for(KeyContext::SymbolOutline, &key) {
        match action {
            Action::PopupMoveDown => outline.move_selection(1),
            Action::PopupMoveUp => outline.move_selection(-1),
            Action::PopupClose => return true,
            Action::PopupSelect => {
                if let Some(row_index) = outline.selected_row_index() {
                    session.jump_to_diff_row(row_index);
                    ensure_diff_cursor_visible(
                        session,
                        current_diff_inner(session, tui_state),
                        tui_state,
                    );
                }
                return true;
            }
            _ => {}
        }
        return false;
    }
    false
}

fn handle_comment_list_key(
    key: KeyEvent,
    list: &mut CommentListState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    tui_state: &mut TuiState,
) -> bool {
    if let Some(action) = keymap.popup_action_for(KeyContext::CommentList, &key) {
        match action {
            Action::PopupMoveDown => {
                list.move_selection(1, session);
                return false;
            }
            Action::PopupMoveUp => {
                list.move_selection(-1, session);
                return false;
            }
            Action::PopupClose => return true,
            Action::PopupSelect => {
                if let Some(id) = list.selected_comment_id(session)
                    && let Some(selection) = session.select_comment_by_id(&id)
                {
                    tui_state.diff_viewport.select_annotation(
                        session,
                        annotation_card::AnnotationSource::Comment { id },
                    );
                    apply_navigation_viewport_placement(
                        session,
                        match selection {
                            CommentSelection::File => NavigationViewportPlacement::Keep,
                            CommentSelection::Diff => NavigationViewportPlacement::Cursor,
                        },
                        tui_state,
                    );
                }
                return true;
            }
            _ => {}
        }
        let transition = tui_state
            .diff_viewport
            .transition_snapshot(session, current_diff_inner(session, tui_state));
        match action {
            Action::CycleCommentState => {
                if let Some(id) = list.selected_comment_id(session)
                    && let Some(state) = session.cycle_comment_state(&id)
                {
                    tui_state.notice = Some(UiNotice {
                        level: UiNoticeLevel::Info,
                        message: format!("comment marked {}", state.label()),
                    });
                }
            }
            Action::CommentListReady => {
                let result = session.ready_all_draft_comments();
                let mut message = format!("readied {} draft comment(s)", result.readied);
                if result.skipped_agent_drafts > 0 {
                    message.push_str(&format!(
                        "; skipped {} agent draft(s) awaiting triage",
                        result.skipped_agent_drafts
                    ));
                }
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message,
                });
            }
            Action::CommentListCycleIntent => {
                if let Some(id) = list.selected_comment_id(session)
                    && let Some(action) = session.cycle_comment_action(&id)
                {
                    tui_state.notice = Some(UiNotice {
                        level: UiNoticeLevel::Info,
                        message: format!(
                            "comment action {}",
                            action.map_or("none", action_intent_label)
                        ),
                    });
                }
            }
            Action::CommentListCycleKind => {
                if let Some(id) = list.selected_comment_id(session)
                    && let Some(kind) = session.cycle_comment_kind(&id)
                {
                    tui_state.notice = Some(UiNotice {
                        level: UiNoticeLevel::Info,
                        message: format!(
                            "comment kind {}",
                            kind.map_or("none", comment_kind_label)
                        ),
                    });
                }
            }
            Action::DeleteComment => {
                if let Some(id) = list.selected_comment_id(session) {
                    session.delete_comment(&id);
                    tui_state.state_tombstones.comments.insert(id);
                    list.clamp(session);
                }
            }
            _ => {}
        }
        tui_state.diff_viewport.finish_transition(
            transition,
            session,
            current_diff_inner(session, tui_state),
        );
        return false;
    }
    false
}

fn action_intent_label(action: crate::state::ActionIntent) -> &'static str {
    match action {
        crate::state::ActionIntent::None => "none",
        crate::state::ActionIntent::Fix => "fix",
        crate::state::ActionIntent::Explain => "explain",
        crate::state::ActionIntent::Test => "test",
        crate::state::ActionIntent::FollowUp => "follow-up",
    }
}

fn comment_kind_label(kind: crate::state::CommentKind) -> &'static str {
    match kind {
        crate::state::CommentKind::Note => "note",
        crate::state::CommentKind::Issue => "issue",
        crate::state::CommentKind::Question => "question",
        crate::state::CommentKind::Praise => "praise",
    }
}

fn handle_open_work_key(
    key: KeyEvent,
    list: &mut OpenWorkListState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    tui_state: &mut TuiState,
) -> bool {
    if let Some(action) = keymap.popup_action_for(KeyContext::OpenWork, &key) {
        match action {
            Action::PopupMoveDown => list.move_selection(1),
            Action::PopupMoveUp => list.move_selection(-1),
            Action::PopupClose => return true,
            Action::PopupSelect => {
                if let Some(row) = list.selected_row().cloned()
                    && let Some(placement) = enter_open_work_row(session, &row)
                {
                    if let Some(comment) = session.selected_comment() {
                        tui_state.diff_viewport.select_annotation(
                            session,
                            annotation_card::AnnotationSource::Comment {
                                id: comment.id.clone(),
                            },
                        );
                    }
                    apply_navigation_viewport_placement(session, placement, tui_state);
                }
                return true;
            }
            _ => {}
        }
        return false;
    }
    false
}

fn enter_open_work_row(
    session: &mut ReviewSession,
    row: &OpenWorkRow,
) -> Option<NavigationViewportPlacement> {
    if let Some(comment_id) = row.comment_id() {
        let valid = session.comments.iter().any(|comment| {
            comment.id == comment_id
                && comment.is_located()
                && comment
                    .path
                    .as_ref()
                    .is_some_and(|path| session.files.iter().any(|file| &file.path == path))
        });
        if !valid {
            return None;
        }
        return session
            .select_comment_by_id(comment_id)
            .map(|selection| match selection {
                CommentSelection::File => NavigationViewportPlacement::Keep,
                CommentSelection::Diff => NavigationViewportPlacement::Cursor,
            });
    }
    let OpenWorkRow::ActionItem { id, target, .. } = row else {
        return None;
    };
    if let Some(target) = target.as_ref()
        && let Some(path) = target.file.as_ref()
        && session.files.iter().any(|file| file.path == *path)
    {
        return session
            .jump_to_review_target(target)
            .map(core_navigation_placement);
    }
    let linked_comment = session
        .durable_sessions()
        .iter()
        .flat_map(|durable| durable.action_items.iter())
        .find(|item| item.id == *id)
        .and_then(|item| {
            item.comment_ids.iter().find_map(|comment_id| {
                session.comments.iter().find(|comment| {
                    comment.id == *comment_id
                        && comment.state == crate::state::CommentState::Todo
                        && comment.has_location()
                        && comment
                            .path
                            .as_ref()
                            .is_some_and(|path| session.files.iter().any(|file| &file.path == path))
                })
            })
        })
        .map(|comment| comment.id.clone());
    if let Some(comment_id) = linked_comment {
        session
            .select_comment_by_id(&comment_id)
            .map(|selection| match selection {
                CommentSelection::File => NavigationViewportPlacement::Keep,
                CommentSelection::Diff => NavigationViewportPlacement::Cursor,
            })
    } else {
        None
    }
}

fn handle_activity_key(
    key: KeyEvent,
    list: &mut ActivityListState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    tui_state: &mut TuiState,
) -> bool {
    let len = tui_state.activity.len();
    if let Some(action) = keymap.popup_action_for(KeyContext::Activity, &key) {
        match action {
            Action::PopupMoveDown => list.move_selection(1, len),
            Action::PopupMoveUp => list.move_selection(-1, len),
            Action::PopupClose => return true,
            Action::PopupSelect => {
                if let Some(event) = tui_state.activity.iter().rev().nth(list.selected)
                    && let Some(index) = session
                        .files
                        .iter()
                        .position(|file| event.message.starts_with(&file.path))
                {
                    let transition = tui_state
                        .diff_viewport
                        .transition_snapshot(session, current_diff_inner(session, tui_state));
                    session.select_file_revealed(index);
                    tui_state.diff_viewport.finish_transition(
                        transition,
                        session,
                        current_diff_inner(session, tui_state),
                    );
                    return true;
                }
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no file target for this event".to_owned(),
                });
            }
            _ => {}
        }
        return false;
    }
    false
}

fn handle_walkthrough_list_key(
    key: KeyEvent,
    list: &mut WalkthroughListState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    tui_state: &mut TuiState,
) -> bool {
    if let Some(action) = keymap.popup_action_for(KeyContext::WalkthroughList, &key) {
        match action {
            Action::PopupMoveDown => list.move_selection(1),
            Action::PopupMoveUp => list.move_selection(-1),
            Action::PopupClose => return true,
            Action::PopupSelect => {
                if let Some(step) = selected_walkthrough_step(session, list).cloned()
                    && let Some(placement) = jump_to_walkthrough_step(session, &step)
                {
                    tui_state.diff_viewport.select_annotation(
                        session,
                        annotation_card::AnnotationSource::Walkthrough {
                            step_id: step.id.clone(),
                            part: 0,
                        },
                    );
                    apply_navigation_viewport_placement(session, placement, tui_state);
                }
                return true;
            }
            Action::WalkthroughDelete => {
                if let Some(id) = list.selected_step_id().map(str::to_owned) {
                    if let Some(durable) = session.durable_sessions_mut().iter_mut().find(|s| {
                        s.walkthroughs
                            .iter()
                            .any(|w| w.steps.iter().any(|step| step.id == id))
                    }) {
                        let _ = review::remove_walkthrough_step(durable, &id);
                        tui_state.notice = Some(UiNotice {
                            level: UiNoticeLevel::Info,
                            message: "deleted walkthrough step".to_owned(),
                        });
                    }
                    list.refresh(session);
                }
                return list.step_ids.is_empty();
            }
            Action::WalkthroughMoveDown => {
                move_selected_walkthrough_step(session, list, 1);
            }
            Action::WalkthroughMoveUp => {
                move_selected_walkthrough_step(session, list, -1);
            }
            _ => {}
        }
    }
    false
}

fn selected_walkthrough_step<'a>(
    session: &'a ReviewSession,
    list: &WalkthroughListState,
) -> Option<&'a WalkthroughStep> {
    let id = list.selected_step_id()?;
    session
        .durable_sessions()
        .iter()
        .flat_map(|s| &s.walkthroughs)
        .flat_map(|w| &w.steps)
        .find(|step| step.id == id)
}

fn move_selected_walkthrough_step(
    session: &mut ReviewSession,
    list: &mut WalkthroughListState,
    delta: isize,
) {
    let Some(id) = list.selected_step_id().map(str::to_owned) else {
        return;
    };
    for durable in session.durable_sessions_mut() {
        for walkthrough in &durable.walkthroughs {
            if let Some(index) = walkthrough.steps.iter().position(|step| step.id == id) {
                let to = (index as isize + delta).clamp(0, walkthrough.steps.len() as isize - 1)
                    as usize;
                let _ = review::move_walkthrough_step(durable, &id, to);
                list.refresh(session);
                return;
            }
        }
    }
}

fn jump_to_walkthrough_step(
    session: &mut ReviewSession,
    step: &WalkthroughStep,
) -> Option<NavigationViewportPlacement> {
    let Some(_) = &step.target.file else {
        return None;
    };
    let placement = session.jump_to_review_target(&step.target)?;
    session.focus = Focus::Diff;
    if let Some(owner) = session.selected_walkthrough_card_owner(&step.target) {
        session.jump_to_diff_row(owner);
        return Some(NavigationViewportPlacement::Cursor);
    }
    Some(core_navigation_placement(placement))
}

impl ReviewLoader<'_> {
    fn base_candidates(&self, session: &ReviewSession) -> Result<Vec<crate::jj::JjChangeSummary>> {
        self.jj.change_summaries(&session.repo)
    }

    fn load(&self, session: &mut ReviewSession, target: ReviewTarget) -> Result<()> {
        self.load_with(session, target, false)
    }

    /// Reload for a background refresh of the same review: the session's
    /// view state is preserved (see `replace_diff_preserving_view`).
    fn load_in_place(&self, session: &mut ReviewSession, target: ReviewTarget) -> Result<()> {
        self.load_with(session, target, true)
    }

    fn load_with(
        &self,
        session: &mut ReviewSession,
        target: ReviewTarget,
        preserve_view: bool,
    ) -> Result<()> {
        let target_author = self
            .jj
            .target_author(&session.repo, &target)
            .unwrap_or_default();
        let diff_text = self
            .jj
            .diff(&session.repo, &target)
            .with_context(|| format!("failed to read jj diff for {target}"))?;
        let mut diff = DiffSet::parse(&diff_text)
            .with_context(|| format!("failed to parse jj diff for {target}"))?;
        diff.apply_ignores(&self.ignore_globs)?;
        if preserve_view {
            session.replace_diff_preserving_view(target, diff);
        } else {
            session.replace_diff(target, diff);
        }
        session.set_target_author(target_author);
        session.annotate_generated_where(|file| {
            self.generated_matcher.is_match(&file.path)
                || crate::generated::diff_content_looks_generated(&file.diff)
        });
        reload_stream_chapter_metadata(self, session);
        Ok(())
    }
}

fn load_review_target(
    review_loader: &ReviewLoader<'_>,
    session: &mut ReviewSession,
    target: ReviewTarget,
    tui_state: &mut TuiState,
) {
    let attention_focus = tui_state.suspend_attention_focus_for_load(session);
    match review_loader.load(session, target.clone()) {
        Ok(()) => {
            tui_state.diff_viewport.reset(session);
            if let Some(state) = attention_focus.clone() {
                tui_state.resume_attention_focus_after_load(session, state);
            }
            reconcile_present_spotlight(session, tui_state);
            // Re-raise the large-change nudge when the newly loaded target
            // is itself big and unorganized.
            let message = match session.large_change_nudge() {
                Some(nudge) => format!("loaded {target} — {nudge}"),
                None => format!("loaded {target}"),
            };
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message,
            });
        }
        Err(error) => {
            if let Some(state) = attention_focus {
                tui_state.resume_attention_focus_after_load(session, state);
            }
            reconcile_present_spotlight(session, tui_state);
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: format!("failed to load {target}: {error:?}"),
            });
        }
    }
}

fn handle_comment_action(
    action: Action,
    session: &mut ReviewSession,
    editor: &mut CommentEditor,
    target: &CommentInputTarget,
    tui_state: &mut TuiState,
) -> bool {
    match action {
        Action::CancelComment => return true,
        Action::SubmitComment => {
            let channel = editor.channel;
            let body = editor.text.clone();
            if let CommentInputTarget::AcceptDraft { id } = target {
                return accept_agent_draft_with_channel(
                    session,
                    tui_state,
                    id,
                    Some(body),
                    channel,
                );
            }
            let transition = tui_state
                .diff_viewport
                .transition_snapshot(session, current_diff_inner(session, tui_state));
            let saved = match target {
                CommentInputTarget::New => {
                    let source_comment_id = (channel == Channel::Delegation)
                        .then(|| selected_onboarding_comment_id(session, tui_state))
                        .flatten();
                    session.add_comment_in_channel_linked(body, channel, source_comment_id)
                }
                CommentInputTarget::NewGeneral => {
                    session.add_general_comment_in_channel(body, channel)
                }
                CommentInputTarget::Edit { id } => {
                    session.update_comment_body_and_channel(id, body, channel)
                }
                CommentInputTarget::AcceptDraft { .. } => unreachable!("handled above"),
            };
            tui_state.diff_viewport.finish_transition(
                transition,
                session,
                current_diff_inner(session, tui_state),
            );
            if saved {
                return true;
            }
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: "comment was not saved; body must contain non-whitespace text".to_owned(),
            });
        }
        Action::InsertNewline => editor.insert_newline(),
        Action::DeleteChar => {
            editor.backspace();
        }
        Action::CycleCommentChannel => editor.cycle_channel(),
        _ => {}
    }
    false
}

fn handle_comment_key(key: KeyEvent, editor: &mut CommentEditor) {
    if let Some(command) = editor_command_for(key) {
        apply_editor_command(editor, command);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorCommand {
    Insert(char),
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveLineStart,
    MoveLineEnd,
    MoveWordLeft,
    MoveWordRight,
    Backspace,
    DeleteForward,
    DeleteLineStart,
    DeleteLineEnd,
    DeletePreviousWord,
}

fn editor_command_for(key: KeyEvent) -> Option<EditorCommand> {
    use EditorCommand::*;
    match (key.code, key.modifiers) {
        (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => Some(MoveLineStart),
        (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => Some(MoveLineEnd),
        (KeyCode::Left, KeyModifiers::CONTROL) | (KeyCode::Char('b'), KeyModifiers::ALT) => {
            Some(MoveWordLeft)
        }
        (KeyCode::Right, KeyModifiers::CONTROL) | (KeyCode::Char('f'), KeyModifiers::ALT) => {
            Some(MoveWordRight)
        }
        (KeyCode::Left, _) | (KeyCode::Char('b'), KeyModifiers::CONTROL) => Some(MoveLeft),
        (KeyCode::Right, _) | (KeyCode::Char('f'), KeyModifiers::CONTROL) => Some(MoveRight),
        (KeyCode::Up, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => Some(MoveUp),
        (KeyCode::Down, _) | (KeyCode::Char('n'), KeyModifiers::CONTROL) => Some(MoveDown),
        (KeyCode::Backspace, KeyModifiers::ALT) => Some(DeletePreviousWord),
        (KeyCode::Backspace, _) | (KeyCode::Char('h'), KeyModifiers::CONTROL) => Some(Backspace),
        (KeyCode::Delete, _) | (KeyCode::Char('d'), KeyModifiers::CONTROL) => Some(DeleteForward),
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => Some(DeleteLineStart),
        (KeyCode::Char('k'), KeyModifiers::CONTROL) => Some(DeleteLineEnd),
        (KeyCode::Char('w'), KeyModifiers::CONTROL) => Some(DeletePreviousWord),
        (KeyCode::Char(ch), modifiers) if modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
            Some(Insert(ch))
        }
        _ => None,
    }
}

fn apply_editor_command(editor: &mut CommentEditor, command: EditorCommand) {
    use EditorCommand::*;
    match command {
        Insert(ch) => editor.insert_char(ch),
        MoveLeft => editor.move_left(),
        MoveRight => editor.move_right(),
        MoveUp => editor.move_up(),
        MoveDown => editor.move_down(),
        MoveLineStart => editor.move_to_line_start(),
        MoveLineEnd => editor.move_to_line_end(),
        MoveWordLeft => editor.move_word_left(),
        MoveWordRight => editor.move_word_right(),
        Backspace => editor.backspace(),
        DeleteForward => editor.delete_forward(),
        DeleteLineStart => editor.delete_to_line_start(),
        DeleteLineEnd => editor.delete_to_line_end(),
        DeletePreviousWord => editor.delete_previous_word(),
    }
}

/// How the menu layer disposed of a mouse event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuMouseOutcome {
    /// Not menu business; give the event to the review surface.
    Ignored,
    /// The menu owned the event (including swallowing the click that closed
    /// an open dropdown, per desktop menubar convention).
    Consumed,
    /// A dropdown item dispatched an action that requested quit.
    Quit,
}

/// Pointer handling for the interactive menu bar. Runs before the review
/// surface's [`handle_mouse_event`]; while a dropdown is open the menu owns
/// the pointer outright. Item dispatch goes through [`handle_normal_action`],
/// the exact path the keyboard uses.
fn handle_menu_mouse_event(
    mouse: MouseEvent,
    terminal_size: ratatui::prelude::Size,
    session: &mut ReviewSession,
    mode: &mut Mode,
    keymap: &KeyMap,
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) -> Result<MenuMouseOutcome> {
    if mode.review_pointer_policy() == ReviewPointerPolicy::BlockedByModal {
        // Modal surfaces own the pointer; a dropdown can also never survive a
        // modal opening (defensive: the modal-open hooks already close it).
        tui_state.menu.close();
        return Ok(MenuMouseOutcome::Ignored);
    }
    let full_area = Rect::new(0, 0, terminal_size.width, terminal_size.height);
    let layout = tui_state.review_layout(session, full_area);
    if layout.menu.height == 0 {
        tui_state.menu.close();
        return Ok(MenuMouseOutcome::Ignored);
    }

    let Some(open) = tui_state.menu.open else {
        // Closed bar: only a completed click (mouse-up) on a title opens a
        // dropdown, and never while a diff drag is being released.
        if mouse.kind == MouseEventKind::Up(MouseButton::Left)
            && tui_state.diff_drag.is_none()
            && let Some(index) = menu::title_at(keymap, layout.menu, mouse.column, mouse.row)
        {
            if !menu::dropdown_items(index, keymap).is_empty() {
                tui_state.menu.open_menu(index);
            }
            return Ok(MenuMouseOutcome::Consumed);
        }
        return Ok(MenuMouseOutcome::Ignored);
    };

    let dropdown = menu::dropdown_rect(open, keymap, layout.menu, full_area);
    let items = menu::dropdown_items(open, keymap);
    let over_dropdown = dropdown.is_some_and(|rect| point_in_rect(mouse.column, mouse.row, rect));
    match mouse.kind {
        MouseEventKind::Moved => {
            // Desktop menubar hover: moving over another title switches the
            // open dropdown; moving over rows highlights them.
            if let Some(index) = menu::title_at(keymap, layout.menu, mouse.column, mouse.row) {
                if index != open && !menu::dropdown_items(index, keymap).is_empty() {
                    tui_state.menu.open_menu(index);
                }
            } else {
                tui_state.menu.hovered = dropdown.and_then(|rect| {
                    menu::dropdown_item_at(rect, items.len(), mouse.column, mouse.row)
                });
            }
            Ok(MenuMouseOutcome::Consumed)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(index) = dropdown
                .and_then(|rect| menu::dropdown_item_at(rect, items.len(), mouse.column, mouse.row))
            {
                let action = items[index].action;
                let quit = handle_normal_action(action, session, mode, review_loader, tui_state)?;
                tui_state.menu.close();
                return Ok(if quit {
                    MenuMouseOutcome::Quit
                } else {
                    MenuMouseOutcome::Consumed
                });
            }
            if let Some(index) = menu::title_at(keymap, layout.menu, mouse.column, mouse.row) {
                if index == open {
                    tui_state.menu.close();
                } else if !menu::dropdown_items(index, keymap).is_empty() {
                    tui_state.menu.open_menu(index);
                } else {
                    tui_state.menu.close();
                }
                return Ok(MenuMouseOutcome::Consumed);
            }
            tui_state.menu.close();
            Ok(MenuMouseOutcome::Consumed)
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if !over_dropdown
                && menu::title_at(keymap, layout.menu, mouse.column, mouse.row).is_none()
            {
                // Click-away closes; the closing click is swallowed.
                tui_state.menu.close();
            }
            Ok(MenuMouseOutcome::Consumed)
        }
        // While a dropdown is open the menu owns the pointer: drags and
        // wheel input must not mutate the review underneath it.
        _ => Ok(MenuMouseOutcome::Consumed),
    }
}

fn handle_mouse_event(
    mouse: MouseEvent,
    terminal_size: ratatui::prelude::Size,
    session: &mut ReviewSession,
    mode: &mut Mode,
    tui_state: &mut TuiState,
) {
    if mode.review_pointer_policy() == ReviewPointerPolicy::BlockedByModal {
        tui_state.diff_drag = None;
        return;
    }

    let layout = tui_state.review_layout(
        session,
        Rect::new(0, 0, terminal_size.width, terminal_size.height),
    );
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            handle_left_down(mouse.column, mouse.row, layout, session, tui_state);
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            handle_left_drag(mouse.column, mouse.row, layout, session, tui_state);
        }
        MouseEventKind::Up(MouseButton::Left) => {
            handle_left_up(session, mode, tui_state);
        }
        MouseEventKind::ScrollDown if point_in_rect(mouse.column, mouse.row, layout.diff) => {
            scroll_diff_visual(session, inner_bordered(layout.diff), 3, tui_state);
        }
        MouseEventKind::ScrollUp if point_in_rect(mouse.column, mouse.row, layout.diff) => {
            scroll_diff_visual(session, inner_bordered(layout.diff), -3, tui_state);
        }
        _ => {}
    }
}

fn handle_left_down(
    x: u16,
    y: u16,
    layout: render::UiLayout,
    session: &mut ReviewSession,
    tui_state: &mut TuiState,
) {
    let files_inner = inner_bordered(layout.files);
    if point_in_rect(x, y, files_inner) {
        session.focus = Focus::Files;
        if let Some(row) = row_in_inner(y, files_inner) {
            if session.stream_mode {
                session.select_visible_tree_row(row);
            } else {
                let transition = tui_state
                    .diff_viewport
                    .transition_snapshot(session, inner_bordered(layout.diff));
                session.select_visible_tree_row(row);
                tui_state.diff_viewport.finish_transition(
                    transition,
                    session,
                    inner_bordered(layout.diff),
                );
            }
        }
        tui_state.diff_drag = None;
        return;
    }

    let diff_inner = inner_bordered(layout.diff);
    if point_in_rect(x, y, diff_inner)
        && let Some(visible_row) = row_in_inner(y, diff_inner)
        && let Some(hit) = diff_hit_at_point(session, diff_inner, x, visible_row, tui_state)
    {
        match hit {
            render::DiffPointHit::Code(row_index) => {
                tui_state.diff_viewport.clear_selected_annotation();
                session.clear_diff_range_selection();
                if session.stream_mode {
                    session.select_stream_row(row_index, true);
                } else {
                    session.select_diff_row(row_index);
                }
                let normalized = session.diff_cursor;
                tui_state
                    .diff_viewport
                    .logical_selection(session, diff_inner);
                tui_state.diff_drag = Some(DiffDrag {
                    start_row: normalized,
                    start_file_index: session.selected,
                    current_row: normalized,
                    saw_drag: false,
                });
            }
            render::DiffPointHit::Annotation { owner, source } => {
                session.clear_diff_range_selection();
                if let Some(comment_id) = source.comment_id() {
                    session.select_comment_by_id(comment_id);
                } else {
                    if session.stream_mode {
                        session.select_stream_row(owner, true);
                    } else {
                        session.select_diff_row(owner);
                    }
                }
                tui_state.diff_viewport.select_annotation(session, source);
                tui_state
                    .diff_viewport
                    .logical_selection(session, diff_inner);
                tui_state.diff_drag = None;
            }
        }
    }
}

fn handle_left_drag(
    x: u16,
    y: u16,
    layout: render::UiLayout,
    session: &mut ReviewSession,
    tui_state: &mut TuiState,
) {
    let Some((start_row, start_file_index)) = tui_state
        .diff_drag
        .as_ref()
        .map(|drag| (drag.start_row, drag.start_file_index))
    else {
        return;
    };
    let diff_inner = inner_bordered(layout.diff);
    if point_in_rect(x, y, diff_inner)
        && let Some(visible_row) = row_in_inner(y, diff_inner)
        && let Some(row_index) =
            render::diff_row_at_point(session, diff_inner, x, visible_row, tui_state)
    {
        apply_diff_drag_row(row_index, start_row, start_file_index, session, tui_state);
    }
}

fn apply_diff_drag_row(
    row_index: usize,
    start_row: usize,
    start_file_index: usize,
    session: &mut ReviewSession,
    tui_state: &mut TuiState,
) {
    if session.stream_mode
        && session
            .stream_row_path(row_index)
            .zip(
                session
                    .files
                    .get(start_file_index)
                    .map(|file| file.path.as_str()),
            )
            .is_some_and(|(destination, start)| destination != start)
    {
        session.clear_diff_range_selection();
        tui_state.diff_drag = None;
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Info,
            message: "range selection cancelled at file boundary".to_owned(),
        });
        return;
    }
    if session.stream_mode {
        session.select_stream_row(row_index, true);
    } else {
        session.select_diff_row(row_index);
    }
    let normalized = session.diff_cursor;
    if let Some(drag) = tui_state.diff_drag.as_mut() {
        drag.current_row = normalized;
        drag.saw_drag = true;
    }
    session.set_diff_range_selection(start_row, normalized);
}

fn handle_left_up(session: &mut ReviewSession, mode: &mut Mode, tui_state: &mut TuiState) {
    let Some(drag) = tui_state.diff_drag.take() else {
        return;
    };
    if drag.saw_drag && session.selected_range_anchor().is_some() {
        *mode = Mode::CommentInput {
            editor: CommentEditor::with_channel(
                String::new(),
                inferred_comment_channel(
                    session,
                    tui_state,
                    selected_onboarding_target(session, tui_state),
                    None,
                ),
            ),
            target: CommentInputTarget::New,
        };
    }
}

#[cfg(test)]
pub(super) mod test_support {
    use crate::{app::ReviewSession, diff::DiffSet, jj::ReviewTarget, state::ReviewState};

    pub(crate) fn snapshot_session(diff_text: &str) -> ReviewSession {
        let diff = DiffSet::parse(diff_text).unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.syntax = crate::syntax::SyntaxConfig {
            enabled: false,
            ..crate::syntax::SyntaxConfig::default()
        };
        session
    }
}

#[cfg(test)]
mod tests {
    use super::{test_support::snapshot_session, *};
    use color_eyre::eyre::bail;
    use std::{cell::RefCell, path::Path, rc::Rc};

    use crate::jj::{JjBackend, JjChangeSummary, ReviewTarget};
    use crate::state::{
        ActionIntent, ActionItem, AttentionRegion, Comment, CommentKind, CommentState, Salience,
        SalienceSource,
    };

    fn attention_session(raw: &str, path: &str, salience: Salience) -> ReviewSession {
        let mut session = snapshot_session(raw);
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let mut durable = crate::state::ReviewSession {
            id: "attention-view".into(),
            attention_regions: vec![AttentionRegion {
                target: crate::attention::target_for_diff(&files, path, None, None).unwrap(),
                salience,
                rationale: Some("generated churn".into()),
                source: SalienceSource::Human,
            }],
            ..Default::default()
        };
        durable.target.base = Some(session.target.base.clone());
        durable.target.revision = Some(session.target.rev.clone());
        durable.target.repo = Some(session.canonical_repo().to_owned());
        session.durable_sessions_mut().push(durable);
        session.stream_mode = true;
        session
    }

    fn presentation_session() -> ReviewSession {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,3 @@\n-old_one\n+new_one\n-old_two\n+new_two\n-old_three\n+new_three\n";
        let mut session = attention_session(raw, "a.rs", Salience::Supporting);
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let step = |id: &str, line: usize| WalkthroughStep {
            id: id.into(),
            author: Some(crate::state::Identity::agent()),
            title: Some(id.into()),
            target: crate::attention::target_for_diff(&files, "a.rs", Some(line), None).unwrap(),
            ..Default::default()
        };
        session.durable_sessions_mut()[0].attention_regions.clear();
        session.durable_sessions_mut()[0].walkthroughs = vec![crate::state::Walkthrough {
            id: "presentation".into(),
            steps: vec![step("first", 1), step("second", 3)],
            ..Default::default()
        }];
        crate::attention::sync_agent_attention(&mut session.durable_sessions_mut()[0], &files)
            .unwrap();
        session
    }

    fn stale_fingerprint(target: &mut crate::state::ReviewTarget) {
        match target.anchor.as_mut().unwrap() {
            crate::anchor::CommentAnchor::File {
                diff_fingerprint, ..
            }
            | crate::anchor::CommentAnchor::Line {
                diff_fingerprint, ..
            }
            | crate::anchor::CommentAnchor::Range {
                diff_fingerprint, ..
            } => *diff_fingerprint = "stale-fingerprint".into(),
        }
    }

    #[test]
    fn startup_tour_maps_to_first_spotlight_in_normal_stream_focus() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let mut session = attention_session(raw, "a.rs", Salience::Spotlight);
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(100, 24),
            ..TuiState::default()
        };

        start_stream_presentation(&mut session, &mut tui_state).unwrap();

        assert!(session.stream_mode);
        assert_eq!(session.focus, Focus::Diff);
        assert!(tui_state.attention_focus.is_some());
        assert_eq!(tui_state.presentation.as_ref().unwrap().index, 0);
        assert_eq!(session.selected_file().unwrap().path, "a.rs");
        assert_eq!(present_status(&session, &tui_state)["view"], "focus");
        assert!(present_status(&session, &tui_state).get("phase").is_none());
    }

    #[test]
    fn startup_tour_without_spotlights_keeps_existing_focus_and_reports_notice() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let mut session = snapshot_session(raw);
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(100, 24),
            ..TuiState::default()
        };
        tui_state.enter_attention_focus(&mut session);

        let error = start_stream_presentation(&mut session, &mut tui_state).unwrap_err();
        assert!(error.1.contains("no current Spotlight"));
        assert!(tui_state.attention_focus.is_some());
        assert!(tui_state.presentation.is_none());
        assert_eq!(session.focus, Focus::Diff);
        assert!(session.fold_context);

        let backend = MockJjBackend::with_diff(Ok(raw.into()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        start_startup_tour_or_notice(&mut session, &loader, &mut tui_state);
        assert!(
            tui_state
                .notice
                .as_ref()
                .unwrap()
                .message
                .contains("no current Spotlight")
        );
    }

    #[test]
    fn quit_cleanup_restores_attention_focus_ephemeral_state() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let mut session = snapshot_session(raw);
        session.focus = Focus::Files;
        session.fold_context = false;
        let mut tui_state = TuiState {
            file_pane: FilePaneState {
                explicit_override: Some(true),
                split_percent: 41,
            },
            ..TuiState::default()
        };
        tui_state.enter_attention_focus(&mut session);
        finish_ephemeral_views_on_quit(&mut session, &mut tui_state);
        assert!(tui_state.attention_focus.is_none());
        assert_eq!(tui_state.file_pane.explicit_override, Some(true));
        assert_eq!(tui_state.file_pane.split_percent, 41);
        assert_eq!(session.focus, Focus::Files);
        assert!(!session.fold_context);
    }

    #[test]
    fn stream_presentation_keeps_normal_actions_and_survives_retarget() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let mut session = attention_session(raw, "a.rs", Salience::Spotlight);
        session.focus = Focus::Files;
        session.fold_context = false;
        session.expanded_skim_folds.insert("prior-peek".into());
        let backend = MockJjBackend::with_diff(Ok(raw.into()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut mode = Mode::Normal;
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(100, 24),
            file_pane: FilePaneState {
                explicit_override: Some(true),
                split_percent: 47,
            },
            ..TuiState::default()
        };
        start_stream_presentation(&mut session, &mut tui_state).unwrap();

        handle_normal_action(
            Action::RangeComment,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert!(session.has_active_diff_range());
        assert!(tui_state.presentation.is_some());

        load_review_target(
            &loader,
            &mut session,
            ReviewTarget::new("main", "@"),
            &mut tui_state,
        );
        assert!(tui_state.presentation.is_some());
        assert!(tui_state.attention_focus.is_some());
        assert!(session.stream_mode);
        assert_eq!(session.focus, Focus::Diff);
        assert!(tui_state.presentation.as_ref().unwrap().stale);

        #[cfg(unix)]
        {
            let status = apply_present_command(
                crate::acp::socket::PresentCommand::End,
                &loader,
                &mut session,
                &mode,
                &mut tui_state,
                None,
                None,
            )
            .unwrap();
            assert_eq!(status, json!({ "active": false }));
            assert!(tui_state.presentation.is_none());
            assert!(tui_state.attention_focus.is_none());
            assert_eq!(tui_state.file_pane.explicit_override, Some(true));
            assert_eq!(tui_state.file_pane.split_percent, 47);
            assert!(!session.fold_context);
            assert!(session.expanded_skim_folds.contains("prior-peek"));
        }
    }

    #[test]
    fn presenter_reanchors_identity_when_spotlights_insert_or_reorder() {
        let mut session = presentation_session();
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(100, 24),
            ..TuiState::default()
        };
        start_stream_presentation(&mut session, &mut tui_state).unwrap();
        goto_present_spotlight(&mut session, &mut tui_state, 1).unwrap();
        assert_eq!(
            tui_state.presentation.as_ref().unwrap().identity.step_id,
            "second"
        );

        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        session.durable_sessions_mut()[0].walkthroughs[0]
            .steps
            .insert(
                0,
                WalkthroughStep {
                    id: "inserted".into(),
                    author: Some(crate::state::Identity::agent()),
                    title: Some("inserted".into()),
                    target: crate::attention::target_for_diff(&files, "a.rs", Some(2), None)
                        .unwrap(),
                    ..Default::default()
                },
            );
        crate::attention::sync_agent_attention(&mut session.durable_sessions_mut()[0], &files)
            .unwrap();
        assert!(reconcile_present_spotlight(&mut session, &mut tui_state));
        let presentation = tui_state.presentation.as_ref().unwrap();
        assert_eq!(presentation.identity.step_id, "second");
        assert_eq!(presentation.index, 2);
        assert!(!presentation.stale);

        let second = session.durable_sessions_mut()[0].walkthroughs[0]
            .steps
            .remove(2);
        session.durable_sessions_mut()[0].walkthroughs[0]
            .steps
            .insert(0, second);
        crate::attention::sync_agent_attention(&mut session.durable_sessions_mut()[0], &files)
            .unwrap();
        assert!(reconcile_present_spotlight(&mut session, &mut tui_state));
        let presentation = tui_state.presentation.as_ref().unwrap();
        assert_eq!(presentation.identity.step_id, "second");
        assert_eq!(presentation.index, 0);
    }

    #[test]
    fn presenter_marks_removed_identity_stale_without_numeric_fallback() {
        let mut session = presentation_session();
        let mut tui_state = TuiState::default();
        start_stream_presentation(&mut session, &mut tui_state).unwrap();
        goto_present_spotlight(&mut session, &mut tui_state, 1).unwrap();
        let selected_anchor = session.selected_stream_row().unwrap().anchor;
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        session.durable_sessions_mut()[0].walkthroughs[0]
            .steps
            .retain(|step| step.id != "second");
        crate::attention::sync_agent_attention(&mut session.durable_sessions_mut()[0], &files)
            .unwrap();

        assert!(!reconcile_present_spotlight(&mut session, &mut tui_state));
        let presentation = tui_state.presentation.as_ref().unwrap();
        assert_eq!(presentation.identity.step_id, "second");
        assert_eq!(presentation.index, 1);
        assert!(presentation.stale);
        assert_eq!(
            session.selected_stream_row().unwrap().anchor,
            selected_anchor
        );
        assert_eq!(
            present_status(&session, &tui_state)["current"]["stale"],
            true
        );
    }

    #[test]
    fn presenter_marks_fingerprint_stale_identity_without_moving() {
        let mut session = presentation_session();
        let mut tui_state = TuiState::default();
        start_stream_presentation(&mut session, &mut tui_state).unwrap();
        goto_present_spotlight(&mut session, &mut tui_state, 1).unwrap();
        let selected_anchor = session.selected_stream_row().unwrap().anchor;
        stale_fingerprint(&mut session.durable_sessions_mut()[0].walkthroughs[0].steps[1].target);
        stale_fingerprint(&mut session.durable_sessions_mut()[0].attention_regions[1].target);

        assert!(!reconcile_present_spotlight(&mut session, &mut tui_state));
        assert!(tui_state.presentation.as_ref().unwrap().stale);
        assert_eq!(
            session.selected_stream_row().unwrap().anchor,
            selected_anchor
        );
        assert_eq!(present_status(&session, &tui_state)["slide_count"], 1);
    }

    #[test]
    fn tour_render_uses_focus_stream_and_requires_current_spotlights() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let mut session = attention_session(raw, "a.rs", Salience::Spotlight);
        let backend = MockJjBackend::with_diff(Ok(raw.into()));
        let rendered = render_tour_text(
            &mut session,
            &KeybindingsConfig::default(),
            &backend,
            80,
            20,
            None,
        )
        .unwrap();
        assert!(rendered.contains("slide 1/1"));
        assert!(rendered.contains("a.rs"));
        assert!(session.fold_context, "tour render applies the Focus preset");

        let mut empty = snapshot_session(raw);
        let rendered = render_tour_text(
            &mut empty,
            &KeybindingsConfig::default(),
            &backend,
            80,
            20,
            None,
        )
        .unwrap();
        assert!(rendered.contains("no current Spotlight regions"));
    }

    #[test]
    fn attention_focus_restores_exact_pane_folds_cards_and_viewport_state() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let mut session = attention_session(raw, "a.rs", Salience::Skim);
        session.focus = Focus::Files;
        session.fold_context = false;
        session.expanded_skim_folds.insert("prior-peek".into());
        session.stream_cursor = 0;
        session.stream_scroll = 0;
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(44, 18),
            file_pane: FilePaneState {
                explicit_override: Some(true),
                split_percent: 35,
            },
            ..TuiState::default()
        };
        let prior_pane = tui_state.file_pane;
        let prior_folds = session.expanded_skim_folds.clone();

        tui_state.enter_attention_focus(&mut session);
        assert!(tui_state.attention_focus.is_some());
        assert!(!tui_state.effective_file_pane(&session, 44).visible);
        assert!(session.fold_context);
        assert!(session.expanded_skim_folds.is_empty());
        assert_eq!(session.focus, Focus::Diff);

        tui_state.leave_attention_focus(&mut session);
        assert_eq!(tui_state.file_pane, prior_pane);
        assert_eq!(session.expanded_skim_folds, prior_folds);
        assert!(!session.fold_context);
        assert_eq!(session.focus, Focus::Files);
        assert!(tui_state.effective_file_pane(&session, 44).visible);
    }

    #[test]
    fn attention_focus_keeps_normal_comment_range_search_and_salience_vocabulary() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let mut session = attention_session(raw, "a.rs", Salience::Spotlight);
        let backend = MockJjBackend::with_diff(Ok(raw.into()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(100, 24),
            ..TuiState::default()
        };
        let mut mode = Mode::Normal;
        let anchor_row = session
            .review_stream()
            .rows
            .iter()
            .position(|row| row.anchor.is_some())
            .unwrap();
        session.select_stream_row(anchor_row, false);
        tui_state.enter_attention_focus(&mut session);

        handle_normal_action(
            Action::RangeComment,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert!(session.has_active_diff_range());
        handle_normal_action(
            Action::Comment,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert!(matches!(mode, Mode::CommentInput { .. }));
        mode = Mode::Normal;
        handle_normal_action(
            Action::FileSearch,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert!(matches!(mode, Mode::FileSearch(_)));
        assert!(tui_state.attention_focus.is_some());
    }

    #[test]
    fn focus_spotlight_navigation_repins_the_exact_narration_card() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,3 @@\n-old_one\n+new_one\n middle\n-old_three\n+new_three\n";
        let mut session = attention_session(raw, "a.rs", Salience::Supporting);
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let first = crate::attention::target_for_diff(&files, "a.rs", Some(1), None).unwrap();
        let second = crate::attention::target_for_diff(&files, "a.rs", Some(3), None).unwrap();
        session.durable_sessions_mut()[0].attention_regions = vec![
            AttentionRegion {
                target: first.clone(),
                salience: Salience::Spotlight,
                rationale: Some("first contract".into()),
                source: SalienceSource::Human,
            },
            AttentionRegion {
                target: second.clone(),
                salience: Salience::Spotlight,
                rationale: Some("second contract".into()),
                source: SalienceSource::Human,
            },
        ];
        session.durable_sessions_mut()[0].walkthroughs = vec![crate::state::Walkthrough {
            id: "walk".into(),
            steps: vec![
                WalkthroughStep {
                    id: "first-card".into(),
                    target: first,
                    title: Some("First".into()),
                    ..Default::default()
                },
                WalkthroughStep {
                    id: "second-card".into(),
                    target: second,
                    title: Some("Second".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }];
        let first_row = session
            .review_stream()
            .rows
            .iter()
            .position(|row| {
                row.anchor
                    .as_ref()
                    .and_then(crate::anchor::CommentAnchor::line)
                    == Some(1)
            })
            .unwrap();
        session.select_stream_row(first_row, false);
        let backend = MockJjBackend::with_diff(Ok(raw.into()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut mode = Mode::Normal;
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(100, 24),
            ..TuiState::default()
        };
        tui_state.enter_attention_focus(&mut session);

        assert!(!session.files[0].viewed);
        handle_normal_action(
            Action::AdvanceReview,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert_eq!(
            tui_state.diff_viewport.selected_annotation_source(&session),
            Some(annotation_card::AnnotationSource::Walkthrough {
                step_id: "second-card".into(),
                part: 0,
            })
        );
        assert!(!session.files[0].viewed);
        assert!(tui_state.attention_focus.is_some());
        handle_normal_action(
            Action::AdvanceReview,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert_eq!(
            tui_state
                .notice
                .as_ref()
                .map(|notice| notice.message.as_str()),
            Some("review tour complete")
        );
        assert_eq!(
            tui_state.diff_viewport.selected_annotation_source(&session),
            Some(annotation_card::AnnotationSource::Walkthrough {
                step_id: "second-card".into(),
                part: 0,
            })
        );
        handle_normal_action(
            Action::SpotlightPrevious,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert_eq!(
            tui_state.diff_viewport.selected_annotation_source(&session),
            Some(annotation_card::AnnotationSource::Walkthrough {
                step_id: "first-card".into(),
                part: 0,
            })
        );
    }

    #[test]
    fn attention_focus_reapplies_safely_across_retarget_and_refresh() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let mut session = attention_session(raw, "a.rs", Salience::Skim);
        let backend = MockJjBackend::with_diff(Ok(raw.into()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(100, 24),
            file_pane: FilePaneState {
                explicit_override: Some(true),
                ..FilePaneState::default()
            },
            ..TuiState::default()
        };
        tui_state.enter_attention_focus(&mut session);
        load_review_target(
            &loader,
            &mut session,
            ReviewTarget::new("main", "@"),
            &mut tui_state,
        );
        assert!(tui_state.attention_focus.is_some());
        assert!(session.fold_context);
        assert!(!tui_state.effective_file_pane(&session, 100).visible);

        refresh_current_target(&loader, &mut session, &mut tui_state, "old", "new");
        assert!(tui_state.attention_focus.is_some());
        assert!(session.fold_context);
        tui_state.leave_attention_focus(&mut session);
        assert!(tui_state.effective_file_pane(&session, 100).visible);
    }

    #[test]
    fn focus_refresh_preserves_active_navigation_and_original_restore_viewport() {
        let removed = (1..=30)
            .map(|line| format!("-old_{line}\n"))
            .collect::<String>();
        let added = (1..=30)
            .map(|line| format!("+new_{line}\n"))
            .collect::<String>();
        let raw = format!(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old_a\n+new_a\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1,30 +1,30 @@\n{removed}{added}"
        );
        let mut session = attention_session(&raw, "a.rs", Salience::Skim);
        session.focus = Focus::Diff;
        let underlying_fold = session
            .review_stream()
            .rows
            .iter()
            .position(|row| matches!(row.kind, crate::app::StreamRowKind::SkimFold(_)))
            .unwrap();
        session.select_stream_row(underlying_fold, false);
        session.stream_scroll = underlying_fold as u16;
        let underlying_selected = session.selected;
        let underlying_cursor = session.stream_cursor;
        let underlying_scroll = session.stream_scroll;

        let backend = MockJjBackend::with_diff(Ok(raw.clone()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(100, 18),
            ..TuiState::default()
        };
        tui_state.enter_attention_focus(&mut session);
        let active_b = session
            .review_stream()
            .rows
            .iter()
            .position(|row| {
                row.path.as_deref() == Some("b.rs")
                    && row
                        .anchor
                        .as_ref()
                        .and_then(crate::anchor::CommentAnchor::line)
                        == Some(30)
            })
            .unwrap();
        session.select_stream_row(active_b, false);
        session.stream_scroll = active_b.saturating_sub(1) as u16;
        let active_anchor = session.selected_stream_row().unwrap().anchor;
        let active_scroll = session.stream_scroll;

        refresh_current_target(&loader, &mut session, &mut tui_state, "old", "new");
        assert!(tui_state.attention_focus.is_some());
        assert_eq!(session.selected_file().unwrap().path, "b.rs");
        assert_eq!(session.selected_stream_row().unwrap().anchor, active_anchor);
        assert_eq!(session.stream_scroll, active_scroll);

        tui_state.leave_attention_focus(&mut session);
        assert_eq!(session.selected, underlying_selected);
        assert_eq!(session.stream_cursor, underlying_cursor);
        assert_eq!(session.stream_scroll, underlying_scroll);
        assert!(matches!(
            session.selected_stream_row().unwrap().kind,
            crate::app::StreamRowKind::SkimFold(_)
        ));
    }

    #[test]
    fn attention_glance_acknowledges_selected_and_bulk_and_peeks_in_stream() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old_a\n+new_a\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old_b\n+new_b\n";
        let mut session = attention_session(raw, "a.rs", Salience::Skim);
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        session.durable_sessions_mut()[0]
            .attention_regions
            .push(AttentionRegion {
                target: crate::attention::target_for_diff(&files, "b.rs", None, None).unwrap(),
                salience: Salience::Skim,
                rationale: Some("lockfile churn".into()),
                source: SalienceSource::Human,
            });
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let mut board = GlanceBoardState::new(&session);
        assert_eq!(board.rows.len(), 2);
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(100, 24),
            ..TuiState::default()
        };

        assert!(!handle_attention_glance_key(
            KeyEvent::from(KeyCode::Char('a')),
            &mut board,
            &mut session,
            &keymap,
            &mut tui_state,
        ));
        assert_eq!(board.rows.iter().filter(|row| row.acknowledged).count(), 1);
        assert!(!handle_attention_glance_key(
            KeyEvent::from(KeyCode::Char('A')),
            &mut board,
            &mut session,
            &keymap,
            &mut tui_state,
        ));
        assert!(board.rows.iter().all(|row| row.acknowledged));
        assert!(session.files.iter().all(|file| file.viewed));

        assert!(handle_attention_glance_key(
            KeyEvent::from(KeyCode::Char(' ')),
            &mut board,
            &mut session,
            &keymap,
            &mut tui_state,
        ));
        let expanded = session.expanded_skim_folds.clone();
        assert!(!expanded.is_empty());
        assert!(handle_attention_glance_key(
            KeyEvent::from(KeyCode::Char(' ')),
            &mut board,
            &mut session,
            &keymap,
            &mut tui_state,
        ));
        assert_eq!(
            session.expanded_skim_folds, expanded,
            "glance Space is idempotent and never collapses an existing peek"
        );
    }

    #[test]
    fn glance_selection_tracks_unique_stale_identity_across_reordering() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let mut session = attention_session(raw, "a.rs", Salience::Skim);
        session.durable_sessions_mut()[0].attention_regions[0]
            .target
            .anchor = None;
        let mut agent = session.durable_sessions()[0].attention_regions[0].clone();
        agent.source = SalienceSource::Agent;
        session.durable_sessions_mut()[0]
            .attention_regions
            .push(agent);
        let mut board = GlanceBoardState::new(&session);
        assert_eq!(board.rows.len(), 2);
        board.selected = 1;
        let selected = board.selected().unwrap().id.clone();

        session.durable_sessions_mut()[0]
            .attention_regions
            .reverse();
        board.refresh(&session);
        assert_eq!(board.selected().unwrap().id, selected);
    }

    #[test]
    fn stream_a_only_acknowledges_selected_fold_and_never_marks_all_on_ordinary_rows() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old_a\n+new_a\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old_b\n+new_b\n";
        let mut session = snapshot_session(raw);
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let mut durable = crate::state::ReviewSession {
            id: "stream-a".into(),
            attention_regions: vec![AttentionRegion {
                target: crate::attention::target_for_diff(&files, "a.rs", None, None).unwrap(),
                salience: Salience::Skim,
                rationale: Some("generated churn".into()),
                source: SalienceSource::Heuristic,
            }],
            ..Default::default()
        };
        durable.target.base = Some(session.target.base.clone());
        durable.target.revision = Some(session.target.rev.clone());
        durable.target.repo = Some(session.canonical_repo().to_owned());
        session.durable_sessions_mut().push(durable);
        session.stream_mode = true;
        session.focus = Focus::Diff;
        let backend = MockJjBackend::with_diff(Ok(raw.into()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut mode = Mode::Normal;
        let mut tui_state = TuiState::default();

        let ordinary = session
            .review_stream()
            .rows
            .iter()
            .position(|row| row.path.as_deref() == Some("b.rs") && row.anchor.is_some())
            .unwrap();
        session.select_stream_row(ordinary, false);
        handle_normal_action(
            Action::MarkAllViewed,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert!(session.files.iter().all(|file| !file.viewed));
        assert!(
            tui_state
                .notice
                .as_ref()
                .unwrap()
                .message
                .contains("no files were marked viewed")
        );

        let fold = session
            .review_stream()
            .rows
            .iter()
            .position(|row| matches!(row.kind, crate::app::StreamRowKind::SkimFold(_)))
            .unwrap();
        session.select_stream_row(fold, false);
        handle_normal_action(
            Action::MarkAllViewed,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert!(session.files[0].viewed);
        assert!(!session.files[1].viewed);
        assert_eq!(session.durable_sessions()[0].attention_progress.len(), 1);

        session.stream_mode = false;
        session
            .files
            .iter_mut()
            .for_each(|file| file.viewed = false);
        handle_normal_action(
            Action::MarkAllViewed,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert!(session.files.iter().all(|file| file.viewed));
    }

    #[test]
    fn stream_mouse_drag_cancels_cross_file_ranges_in_unified_and_split_views() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old_a\n+new_a\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old_b\n+new_b\n";
        for split in [false, true] {
            let mut session = snapshot_session(raw);
            session.stream_mode = true;
            session.focus = Focus::Diff;
            if split {
                session.toggle_diff_view();
            }
            let stream = session.review_stream();
            let a = stream
                .rows
                .iter()
                .position(|row| row.path.as_deref() == Some("a.rs") && row.anchor.is_some())
                .unwrap();
            let b = stream
                .rows
                .iter()
                .position(|row| row.path.as_deref() == Some("b.rs") && row.anchor.is_some())
                .unwrap();
            drop(stream);
            session.select_stream_row(a, false);
            session.toggle_diff_range_selection();
            let start = session.diff_cursor;
            let mut tui_state = TuiState {
                diff_drag: Some(DiffDrag {
                    start_row: start,
                    start_file_index: 0,
                    current_row: start,
                    saw_drag: false,
                }),
                ..Default::default()
            };
            apply_diff_drag_row(b, start, 0, &mut session, &mut tui_state);
            assert!(session.diff_range_selection.is_none());
            assert!(tui_state.diff_drag.is_none());
            assert!(
                tui_state
                    .notice
                    .as_ref()
                    .unwrap()
                    .message
                    .contains("file boundary")
            );
            session.select_stream_row(a, false);
            assert!(session.diff_range_selection.is_none());
        }
    }

    #[test]
    fn stream_keyboard_crossing_cancels_range_with_notice() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old_a\n+new_a\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old_b\n+new_b\n";
        let mut session = snapshot_session(raw);
        session.stream_mode = true;
        session.focus = Focus::Diff;
        let last_a = session
            .review_stream()
            .rows
            .iter()
            .rposition(|row| row.path.as_deref() == Some("a.rs") && row.anchor.is_some())
            .unwrap();
        session.select_stream_row(last_a, false);
        session.toggle_diff_range_selection();
        let backend = MockJjBackend::with_diff(Ok(raw.into()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut mode = Mode::Normal;
        let mut tui_state = TuiState::default();
        handle_normal_action(
            Action::MoveDown,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert_eq!(session.selected_file().unwrap().path, "b.rs");
        assert!(session.diff_range_selection.is_none());
        assert!(
            tui_state
                .notice
                .as_ref()
                .unwrap()
                .message
                .contains("file boundary")
        );
    }

    #[test]
    fn every_mode_declares_review_pointer_ownership() {
        let session = snapshot_session("");
        let modes = vec![
            ("normal", Mode::Normal, ReviewPointerPolicy::Enabled),
            ("help", Mode::Help, ReviewPointerPolicy::BlockedByModal),
            (
                "target chooser",
                Mode::TargetChooser(TargetChooserState::new(Vec::new(), "trunk()", "@")),
                ReviewPointerPolicy::BlockedByModal,
            ),
            (
                "revset input",
                Mode::RevsetInput(RevsetInputState::new("trunk()", "@")),
                ReviewPointerPolicy::BlockedByModal,
            ),
            (
                "operation picker",
                Mode::OperationPicker(OperationPickerState::new(Vec::new())),
                ReviewPointerPolicy::BlockedByModal,
            ),
            (
                "jj helpers",
                Mode::JjHelpers(JjHelperState::for_session(&session)),
                ReviewPointerPolicy::BlockedByModal,
            ),
            (
                "flags",
                Mode::FlagList(FlagListState::new(&session)),
                ReviewPointerPolicy::BlockedByModal,
            ),
            (
                "open work",
                Mode::OpenWork(OpenWorkListState::new(&session)),
                ReviewPointerPolicy::BlockedByModal,
            ),
            (
                "activity",
                Mode::Activity(ActivityListState::new()),
                ReviewPointerPolicy::BlockedByModal,
            ),
            (
                "drafts",
                Mode::DraftList(DraftListState::new(&session)),
                ReviewPointerPolicy::BlockedByModal,
            ),
            (
                "file search",
                Mode::FileSearch(FileSearchState::new(&session)),
                ReviewPointerPolicy::BlockedByModal,
            ),
            (
                "outline",
                Mode::SymbolOutline(SymbolOutlineState::new(&session)),
                ReviewPointerPolicy::BlockedByModal,
            ),
            (
                "comments",
                Mode::CommentList(CommentListState::default()),
                ReviewPointerPolicy::BlockedByModal,
            ),
            (
                "view options",
                Mode::ViewOptions(ViewOptionsState::default()),
                ReviewPointerPolicy::BlockedByModal,
            ),
            (
                "walkthrough",
                Mode::WalkthroughList(WalkthroughListState::new(&session)),
                ReviewPointerPolicy::BlockedByModal,
            ),
            (
                "comment input",
                Mode::CommentInput {
                    editor: CommentEditor::default(),
                    target: CommentInputTarget::New,
                },
                ReviewPointerPolicy::BlockedByModal,
            ),
        ];

        for (name, mode, expected) in modes {
            assert_eq!(mode.review_pointer_policy(), expected, "{name}");
        }
    }

    #[test]
    fn local_editor_key_events_map_to_table_driven_commands() {
        let cases = [
            (KeyEvent::from(KeyCode::Home), EditorCommand::MoveLineStart),
            (KeyEvent::from(KeyCode::End), EditorCommand::MoveLineEnd),
            (
                KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
                EditorCommand::MoveLineStart,
            ),
            (
                KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL),
                EditorCommand::MoveLineEnd,
            ),
            (
                KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
                EditorCommand::MoveLeft,
            ),
            (
                KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
                EditorCommand::MoveRight,
            ),
            (
                KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
                EditorCommand::MoveUp,
            ),
            (
                KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
                EditorCommand::MoveDown,
            ),
            (
                KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
                EditorCommand::Backspace,
            ),
            (
                KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
                EditorCommand::DeleteForward,
            ),
            (
                KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                EditorCommand::DeleteLineStart,
            ),
            (
                KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
                EditorCommand::DeleteLineEnd,
            ),
            (
                KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
                EditorCommand::DeletePreviousWord,
            ),
            (
                KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
                EditorCommand::MoveWordLeft,
            ),
            (
                KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT),
                EditorCommand::MoveWordRight,
            ),
            (
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT),
                EditorCommand::DeletePreviousWord,
            ),
            (KeyEvent::from(KeyCode::Left), EditorCommand::MoveLeft),
            (KeyEvent::from(KeyCode::Right), EditorCommand::MoveRight),
            (KeyEvent::from(KeyCode::Up), EditorCommand::MoveUp),
            (KeyEvent::from(KeyCode::Down), EditorCommand::MoveDown),
            (
                KeyEvent::from(KeyCode::Char('界')),
                EditorCommand::Insert('界'),
            ),
        ];

        for (key, expected) in cases {
            assert_eq!(editor_command_for(key), Some(expected), "{key:?}");
        }
        assert_eq!(editor_command_for(KeyEvent::from(KeyCode::Enter)), None);
    }

    #[derive(Clone, Default)]
    struct SharedWriter(Rc<RefCell<Vec<u8>>>);

    impl io::Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn terminal_lifecycle_drop_restores_on_post_acquisition_error() {
        let writer = SharedWriter::default();
        let output = writer.0.clone();
        let backend = CrosstermBackend::new(writer);
        let mut terminal = Terminal::new(backend).expect("terminal");

        {
            let _lifecycle = TerminalLifecycle::new(&mut terminal);
            // Once terminal acquisition has succeeded, any later error return must
            // restore the terminal through Drop.
        }

        let output = output.borrow();
        let output = String::from_utf8_lossy(&output);
        assert!(
            output.contains("\u{1b}[?1049l"),
            "dropping the lifecycle should leave the alternate screen; output={output:?}"
        );
        assert!(
            output.contains("\u{1b}[?1000l"),
            "dropping the lifecycle should disable mouse capture; output={output:?}"
        );
    }

    #[test]
    fn terminal_lifecycle_explicit_restore_makes_drop_a_noop() {
        let writer = SharedWriter::default();
        let output = writer.0.clone();
        let backend = CrosstermBackend::new(writer);
        let mut terminal = Terminal::new(backend).expect("terminal");

        {
            let mut lifecycle = TerminalLifecycle::new(&mut terminal);
            // Explicit restoration should make Drop a no-op so normal quit and
            // handled error paths do not emit duplicate cleanup sequences.
            lifecycle.restore().expect("restore terminal");
        }

        let output = output.borrow();
        let output = String::from_utf8_lossy(&output);
        assert_eq!(
            output.matches("\u{1b}[?1049l").count(),
            1,
            "explicit restore should leave the alternate screen exactly once; output={output:?}"
        );
        assert_eq!(
            output.matches("\u{1b}[?1000l").count(),
            1,
            "explicit restore should disable mouse capture exactly once; output={output:?}"
        );
    }

    struct MockJjBackend {
        snapshot_calls: RefCell<usize>,
        calls: RefCell<Vec<ReviewTarget>>,
        diff_text: Result<String, String>,
        diff_queue: RefCell<Vec<Result<String, String>>>,
        summaries: Vec<JjChangeSummary>,
        stack: Vec<JjChangeSummary>,
        operations: Vec<crate::jj::JjOperationSummary>,
        diff_at_op: Option<String>,
        file_contents: Option<String>,
        commands: RefCell<Vec<Vec<String>>>,
        command_result: Result<String, String>,
        fingerprint: RefCell<Result<String, String>>,
    }

    impl MockJjBackend {
        fn with_diff(diff_text: Result<String, String>) -> Self {
            Self {
                snapshot_calls: RefCell::new(0),
                calls: RefCell::new(Vec::new()),
                diff_text,
                diff_queue: RefCell::new(Vec::new()),
                summaries: Vec::new(),
                stack: Vec::new(),
                operations: Vec::new(),
                diff_at_op: None,
                file_contents: None,
                commands: RefCell::new(Vec::new()),
                command_result: Ok(String::new()),
                fingerprint: RefCell::new(Ok(String::new())),
            }
        }
    }

    #[test]
    fn yank_handoff_copies_agent_markdown_and_sets_notice() {
        let mut session = snapshot_session("diff --git a/src/lib.rs b/src/lib.rs\n");
        session.comments.push(Comment {
            id: "c1".to_owned(),
            path: Some("src/lib.rs".to_owned()),
            line: Some(1),
            end_line: None,
            anchor: None,
            body: "fix this".to_owned(),
            kind: Some(CommentKind::Issue),
            action: Some(ActionIntent::Fix),
            state: CommentState::Todo,
            channel: Channel::Delegation,
            created_at: chrono::Utc::now(),
            ..Default::default()
        });
        session.comments.push(Comment {
            id: "private".to_owned(),
            body: "withheld draft".to_owned(),
            state: CommentState::Draft,
            created_at: chrono::Utc::now(),
            ..Default::default()
        });
        ensure_tui_review_session(&mut session)
            .action_items
            .push(ActionItem {
                id: "t1".to_owned(),
                title: "do the thing".to_owned(),
                action: Some(ActionIntent::Fix),
                ..ActionItem::default()
            });
        let copied = RefCell::new(String::new());
        let mut tui_state = TuiState::default();
        yank_handoff_with(&session, &mut tui_state, |body| {
            copied.replace(body.to_owned());
            Ok(ClipboardMethod::Osc52)
        });

        assert!(copied.borrow().starts_with("# Human review handoff"));
        assert!(copied.borrow().contains("fix this"));
        assert!(!copied.borrow().contains("withheld draft"));
        assert_eq!(
            tui_state.notice,
            Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: "handoff copied via OSC52 (/dev/tty) (1 todo comments included, 1 drafts withheld; 2 action items)".to_owned(),
            })
        );
    }

    #[test]
    fn open_work_enter_prefers_an_action_item_target() {
        let mut session = snapshot_session(
            "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/src/b.rs b/src/b.rs\n--- a/src/b.rs\n+++ b/src/b.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let row = OpenWorkRow::ActionItem {
            id: "item".into(),
            title: "Go to b".into(),
            action: Some(ActionIntent::Fix),
            target: Box::new(Some(crate::state::ReviewTarget {
                file: Some("src/b.rs".into()),
                line: Some(1),
                ..crate::state::ReviewTarget::default()
            })),
        };

        let _ = enter_open_work_row(&mut session, &row);

        assert_eq!(session.selected_file().unwrap().path, "src/b.rs");
        assert_eq!(session.focus, Focus::Diff);
    }

    #[test]
    fn open_work_enter_uses_linked_location_and_comment_rows_enter_comments() {
        let mut session = snapshot_session(
            "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        session.toggle_focus();
        session.add_comment("first evidence".into());
        session.add_comment("second feedback".into());
        session.comments[0].id = "first".into();
        session.comments[1].id = "second".into();
        let durable_id = session.comments[0].session_id.clone().unwrap();
        session
            .durable_sessions_mut()
            .iter_mut()
            .find(|durable| durable.id == durable_id)
            .unwrap()
            .action_items
            .push(ActionItem {
                id: "item".into(),
                title: "Use evidence".into(),
                comment_ids: vec!["first".into()],
                ..ActionItem::default()
            });

        let _ = enter_open_work_row(
            &mut session,
            &OpenWorkRow::ActionItem {
                id: "item".into(),
                title: "Use evidence".into(),
                action: None,
                target: Box::new(None),
            },
        );
        assert_eq!(session.selected_comment_index(), Some(0));

        let _ = enter_open_work_row(
            &mut session,
            &OpenWorkRow::EvidenceComment {
                id: "second".into(),
            },
        );
        assert_eq!(session.selected_comment_index(), Some(1));

        let _ = enter_open_work_row(
            &mut session,
            &OpenWorkRow::TodoComment { id: "first".into() },
        );
        assert_eq!(session.selected_comment_index(), Some(0));
    }

    #[test]
    fn configured_tui_creation_defaults_to_todo_and_can_default_to_draft() {
        let diff = crate::diff::DiffSet::parse(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
        let mut session = ReviewSession::new_with_config(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff.clone(),
            ReviewState::default(),
            &crate::config::Config::default(),
        );
        session.add_comment("ready feedback".to_owned());
        assert_eq!(session.comments[0].state, CommentState::Todo);
        assert!(session.comments[0].session_id.is_some());

        let mut config = crate::config::Config::default();
        config.comments.initial_state = crate::config::InitialCommentState::Draft;
        let mut session = ReviewSession::new_with_config(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
            &config,
        );
        session.add_comment("private feedback".to_owned());
        assert_eq!(session.comments[0].state, CommentState::Draft);
    }

    #[test]
    fn tui_new_and_general_flows_store_inferred_channels_at_creation() {
        let diff = crate::diff::DiffSet::parse(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
        let mut config = crate::config::Config::default();
        config.identity.name = Some("Reviewer".into());
        let mut session = ReviewSession::new_with_config(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
            &config,
        );
        session.set_target_author(crate::jj::TargetAuthor {
            name: Some("Reviewer".into()),
            email: None,
        });
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState {
            agent_contacted: true,
            ..TuiState::default()
        };
        let mut mode = Mode::Normal;

        handle_normal_action(
            Action::Comment,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        let Mode::CommentInput { editor, target } = &mut mode else {
            panic!("comment action should open editor");
        };
        assert_eq!(editor.channel, Channel::Delegation);
        editor.text = "delegate this".into();
        editor.cursor = editor.text.len();
        assert!(handle_comment_action(
            Action::SubmitComment,
            &mut session,
            editor,
            target,
            &mut tui_state,
        ));
        assert_eq!(session.comments[0].channel, Channel::Delegation);
        assert_eq!(session.comments[0].state, CommentState::Todo);

        session.set_target_author(crate::jj::TargetAuthor {
            name: Some("Teammate".into()),
            email: None,
        });
        let channel = inferred_comment_channel(&session, &tui_state, false, None);
        let mut editor = CommentEditor::with_channel(String::new(), channel);
        let target = CommentInputTarget::NewGeneral;
        editor.text = "team feedback".into();
        editor.cursor = editor.text.len();
        assert!(handle_comment_action(
            Action::SubmitComment,
            &mut session,
            &mut editor,
            &target,
            &mut tui_state,
        ));
        assert!(session.comments[1].is_general());
        assert_eq!(session.comments[1].channel, Channel::Collaboration);
        assert_eq!(session.comments[1].state, CommentState::Todo);
    }

    #[test]
    fn configured_agent_identity_alone_is_not_attachment_evidence() {
        let diff = crate::diff::DiffSet::parse(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
        let mut config = crate::config::Config::default();
        config.identity.name = Some("Reviewer".into());
        // `[agent] name` is identity config, not evidence that any agent is
        // attached to the session (docs/decisions.md D9).
        config.agent.name = Some("configured-but-not-attached".into());
        let mut session = ReviewSession::new_with_config(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
            &config,
        );
        session.set_target_author(crate::jj::TargetAuthor {
            name: Some("Reviewer".into()),
            email: None,
        });
        let tui_state = TuiState::default();

        assert_eq!(
            inferred_comment_channel(&session, &tui_state, false, None),
            Channel::Note
        );
    }

    #[test]
    fn tui_range_and_edit_flows_fallback_private_preserve_then_cycle_channel() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,2 @@\n-old\n+new\n+more\n",
        );
        session.toggle_focus();
        session.set_diff_range_selection(2, 3);
        let mut tui_state = TuiState::default();
        let channel = inferred_comment_channel(&session, &tui_state, false, None);
        assert_eq!(channel, Channel::Note);
        session.add_comment_in_channel("private range".into(), channel);
        assert_eq!(session.comments[0].channel, Channel::Note);
        assert_eq!(session.comments[0].state, CommentState::Draft);

        session.comments[0].channel = Channel::Collaboration;
        let id = session.comments[0].id.clone();
        let mut editor = CommentEditor::with_channel(
            session.comments[0].body.clone(),
            session.comments[0].channel,
        );
        let target = CommentInputTarget::Edit { id: id.clone() };
        assert!(handle_comment_action(
            Action::SubmitComment,
            &mut session,
            &mut editor,
            &target,
            &mut tui_state,
        ));
        assert_eq!(session.comments[0].channel, Channel::Collaboration);

        let mut editor = CommentEditor::with_channel(
            session.comments[0].body.clone(),
            session.comments[0].channel,
        );
        assert!(!handle_comment_action(
            Action::CycleCommentChannel,
            &mut session,
            &mut editor,
            &target,
            &mut tui_state,
        ));
        assert_eq!(editor.channel, Channel::Note);
        assert!(handle_comment_action(
            Action::SubmitComment,
            &mut session,
            &mut editor,
            &target,
            &mut tui_state,
        ));
        assert_eq!(session.comments[0].channel, Channel::Note);
        assert_eq!(session.comments[0].state, CommentState::Draft);
    }

    #[test]
    fn failed_private_channel_edit_is_atomic_and_keeps_editor_open() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        );
        session.toggle_focus();
        assert!(session.add_comment_in_channel("actionable".into(), Channel::Delegation));
        let id = session.comments[0].id.clone();
        session.comments[0].state = CommentState::Todo;
        let original = session.comments[0].clone();
        let mut mode = Mode::CommentInput {
            editor: CommentEditor::with_channel("   ".into(), Channel::Note),
            target: CommentInputTarget::Edit { id: id.clone() },
        };
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let mut tui_state = TuiState::default();

        assert!(
            !handle_key_event(
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
                &mut session,
                &mut mode,
                &keymap,
                &loader,
                &mut tui_state,
            )
            .unwrap()
        );
        let Mode::CommentInput { editor, .. } = &mut mode else {
            panic!("failed edit must keep the editor open");
        };
        assert_eq!(editor.text, "   ");
        assert_eq!(editor.channel, Channel::Note);
        assert_eq!(session.comments[0], original);
        assert!(tui_state.notice.as_ref().is_some_and(|notice| {
            notice.level == UiNoticeLevel::Error && notice.message.contains("not saved")
        }));

        editor.text = "private replacement".into();
        editor.cursor = editor.text.len();
        assert!(
            !handle_key_event(
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
                &mut session,
                &mut mode,
                &keymap,
                &loader,
                &mut tui_state,
            )
            .unwrap()
        );
        assert!(matches!(mode, Mode::Normal));
        assert_eq!(session.comments[0].body, "private replacement");
        assert_eq!(session.comments[0].channel, Channel::Note);
        assert_eq!(session.comments[0].state, CommentState::Draft);
    }

    #[test]
    fn composing_on_onboarding_annotation_creates_new_delegation_without_mutating_card() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let onboarding = session
            .add_agent_draft("a.txt".into(), Some(1), "agent narration".into())
            .unwrap();
        session.select_comment_by_id(&onboarding.id);
        let mut tui_state = TuiState::default();
        assert!(selected_onboarding_target(&session, &tui_state));
        let mut editor = CommentEditor::with_channel(
            String::new(),
            inferred_comment_channel(&session, &tui_state, true, None),
        );
        editor.text = "please change this".into();
        editor.cursor = editor.text.len();

        assert!(handle_comment_action(
            Action::SubmitComment,
            &mut session,
            &mut editor,
            &CommentInputTarget::New,
            &mut tui_state,
        ));

        let original = session
            .comments
            .iter()
            .find(|comment| comment.id == onboarding.id)
            .unwrap();
        assert_eq!(original.channel, Channel::Onboarding);
        assert_eq!(original.body, "agent narration");
        let request = session
            .comments
            .iter()
            .find(|comment| comment.id != onboarding.id)
            .unwrap();
        assert_eq!(request.channel, Channel::Delegation);
        assert_eq!(request.anchor, original.anchor);
        assert_eq!(
            request.source_comment_id.as_deref(),
            Some(onboarding.id.as_str())
        );
        assert_eq!(
            serde_json::to_value(request).unwrap()["source_comment_id"],
            onboarding.id
        );
    }

    #[test]
    fn walkthrough_onboarding_inference_requires_explicit_agent_author() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let target = crate::attention::target_for_diff(&files, "a.txt", Some(1), None).unwrap();
        let durable = ensure_tui_review_session(&mut session);
        durable.walkthroughs.push(crate::state::Walkthrough {
            id: "walk".into(),
            steps: vec![WalkthroughStep {
                id: "step".into(),
                author: Some(crate::state::Identity {
                    kind: AuthorKind::Agent,
                    name: "configured-agent".into(),
                }),
                target: target.clone(),
                ..Default::default()
            }],
            ..Default::default()
        });
        durable
            .attention_regions
            .push(crate::state::AttentionRegion {
                target,
                salience: crate::state::Salience::Spotlight,
                rationale: None,
                source: crate::state::SalienceSource::Agent,
            });
        let owner = session
            .selected_walkthrough_card_owner(
                &session.durable_sessions()[0].walkthroughs[0].steps[0].target,
            )
            .unwrap();
        session.select_diff_row(owner);
        let tui_state = TuiState::default();
        tui_state.diff_viewport.select_annotation(
            &session,
            annotation_card::AnnotationSource::Walkthrough {
                step_id: "step".into(),
                part: 0,
            },
        );
        assert!(selected_onboarding_target(&session, &tui_state));
        assert_eq!(
            inferred_comment_channel(
                &session,
                &tui_state,
                selected_onboarding_target(&session, &tui_state),
                None,
            ),
            Channel::Delegation
        );

        session.durable_sessions_mut()[0].walkthroughs[0].steps[0].author =
            Some(crate::state::Identity {
                kind: AuthorKind::Human,
                name: "Ada".into(),
            });
        assert!(!selected_onboarding_target(&session, &tui_state));
        assert_eq!(
            inferred_comment_channel(
                &session,
                &tui_state,
                selected_onboarding_target(&session, &tui_state),
                None,
            ),
            Channel::Note
        );
        session.durable_sessions_mut()[0].walkthroughs[0].steps[0].author = None;
        assert!(!selected_onboarding_target(&session, &tui_state));
    }

    #[test]
    fn tui_walkthrough_steps_stamp_configured_human_and_current_anchor() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        );
        session.human_identity = crate::state::Identity {
            kind: AuthorKind::Human,
            name: "Configured Human".into(),
        };
        session.toggle_focus();
        let row = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.new_lineno == Some(1))
            .unwrap();
        session.select_diff_row(row);
        add_walkthrough_step_from_selection(&mut session).unwrap();
        let durable = &session.durable_sessions()[0];
        let step = &durable.walkthroughs[0].steps[0];
        assert_eq!(step.author.as_ref().unwrap().name, "Configured Human");
        assert!(step.target.anchor.is_some());
        assert_eq!(durable.attention_regions.len(), 1);
        assert_eq!(
            durable.attention_regions[0].source,
            crate::state::SalienceSource::Human
        );
    }

    #[test]
    fn walkthrough_jump_uses_the_same_right_side_range_card_owner() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,2 @@\n-old\n+new one\n+new two\n",
        );
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let target = crate::attention::target_for_diff(&files, "a.txt", Some(1), Some(2)).unwrap();
        let expected = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "new two")
            .unwrap();
        let step = WalkthroughStep {
            target,
            ..Default::default()
        };
        assert_eq!(
            jump_to_walkthrough_step(&mut session, &step),
            Some(NavigationViewportPlacement::Cursor)
        );
        assert_eq!(session.diff_cursor, expected);
    }

    #[test]
    fn clicking_second_colocated_card_targets_state_edit_and_delete_exactly() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        );
        session.file_pane_visible = false;
        session.toggle_focus();
        session.add_comment("first card".into());
        session.add_comment("second card".into());
        session.comments[0].id = "first".into();
        session.comments[1].id = "second".into();
        let size = ratatui::prelude::Size::new(100, 30);
        let mut tui_state = TuiState {
            terminal_size: size,
            ..TuiState::default()
        };
        let area = Rect::new(0, 0, size.width, size.height);
        let layout = tui_state.review_layout(&session, area);
        let inner = inner_bordered(layout.diff);
        let source = annotation_card::AnnotationSource::Comment {
            id: "second".into(),
        };
        let visible = tui_state
            .diff_viewport
            .annotation_visible_row(&session, inner, &source)
            .expect("second card visible");
        let mut mode = Mode::Normal;
        handle_mouse_event(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: inner.x + 4,
                row: inner.y + visible as u16,
                modifiers: KeyModifiers::NONE,
            },
            size,
            &mut session,
            &mut mode,
            &mut tui_state,
        );
        assert_eq!(session.selected_comment().unwrap().id, "second");

        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        handle_normal_action(
            Action::CycleCommentState,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert_eq!(session.comments[0].state, CommentState::Draft);
        assert_eq!(session.comments[1].state, CommentState::Todo);

        handle_normal_action(
            Action::EditComment,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert!(matches!(
            mode,
            Mode::CommentInput {
                target: CommentInputTarget::Edit { ref id },
                ..
            } if id == "second"
        ));
        mode = Mode::Normal;
        handle_normal_action(
            Action::DeleteComment,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert_eq!(session.comments.len(), 1);
        assert_eq!(session.comments[0].id, "first");
    }

    #[test]
    fn empty_comment_center_creates_general_comment_and_readies_drafts() {
        let mut session = snapshot_session("diff --git a/a.txt b/a.txt\n");
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let mut mode = Mode::Normal;
        let mut tui_state = TuiState::default();
        handle_normal_action(
            Action::CommentList,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert!(matches!(mode, Mode::CommentList(_)));

        handle_key_event(
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            &mut session,
            &mut mode,
            &keymap,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        let Mode::CommentInput { editor, target } = &mut mode else {
            panic!("n should open general comment input");
        };
        assert_eq!(*target, CommentInputTarget::NewGeneral);
        editor.text = "general draft".to_owned();
        editor.cursor = editor.text.len();
        assert!(handle_comment_action(
            Action::SubmitComment,
            &mut session,
            editor,
            target,
            &mut tui_state,
        ));
        assert!(session.comments[0].is_general());
        assert_eq!(session.comments[0].state, CommentState::Draft);

        let mut list = CommentListState::default();
        assert!(!handle_comment_list_key(
            KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE),
            &mut list,
            &mut session,
            &keymap,
            &mut tui_state,
        ));
        assert_eq!(session.comments[0].state, CommentState::Todo);
        assert!(
            tui_state
                .notice
                .as_ref()
                .is_some_and(|notice| notice.message.contains("readied 1"))
        );
    }

    #[test]
    fn comment_mode_routes_local_ctrl_d_through_the_editor_command_map() {
        let family = "👨‍👩‍👧‍👦";
        let mut session = snapshot_session("diff --git a/a.txt b/a.txt\n");
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let mut mode = Mode::CommentInput {
            editor: CommentEditor::with_cursor_for_test(&format!("a{family}b"), 1),
            target: CommentInputTarget::New,
        };

        handle_key_event(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            &mut session,
            &mut mode,
            &keymap,
            &loader,
            &mut TuiState::default(),
        )
        .unwrap();

        let Mode::CommentInput { editor, .. } = mode else {
            panic!("comment mode should remain open");
        };
        assert_eq!(editor.text, "ab");
        assert_eq!(editor.cursor, 1);
    }

    #[test]
    fn inline_comment_state_cycles_from_diff_key() {
        let mut session =
            snapshot_session("diff --git a/src/lib.rs b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n");
        session.add_comment("draft note".to_owned());
        let mut tui_state = TuiState::default();
        let mut mode = Mode::Normal;

        handle_normal_action(
            Action::CycleCommentState,
            &mut session,
            &mut mode,
            &ReviewLoader {
                ignore_globs: Vec::new(),
                generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
                jj: &MockJjBackend::with_diff(Ok(String::new())),
            },
            &mut tui_state,
        )
        .unwrap();

        assert_eq!(session.comments[0].state, CommentState::Todo);
        assert_eq!(
            tui_state.notice.map(|notice| notice.message),
            Some("comment state: todo".to_owned())
        );
    }

    #[test]
    fn refresh_events_include_latest_operation_description() {
        let mut session =
            snapshot_session("diff --git a/queue.rs b/queue.rs\n@@ -1 +1 @@\n-old\n+new\n");
        let mut backend = MockJjBackend::with_diff(Ok(
            "diff --git a/queue.rs b/queue.rs\n@@ -1 +1 @@\n-old\n+newer\n".to_owned(),
        ));
        backend.operations = vec![crate::jj::JjOperationSummary {
            operation_id: "abc".to_owned(),
            time: "now".to_owned(),
            description: "snapshot working copy".to_owned(),
        }];
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();

        refresh_current_target(
            &loader,
            &mut session,
            &mut tui_state,
            "@ old old",
            "@ new new",
        );

        assert!(
            tui_state
                .activity
                .iter()
                .any(|event| event.message.contains("op: snapshot working copy"))
        );
    }

    #[test]
    fn latest_operation_description_truncates_embedded_operation_ids() {
        let mut backend = MockJjBackend::with_diff(Ok(String::new()));
        let long_id = "4a3b2c1d9e0f".to_owned() + &"a".repeat(116);
        backend.operations = vec![crate::jj::JjOperationSummary {
            operation_id: long_id.clone(),
            time: "now".to_owned(),
            description: format!("undo operation {long_id}"),
        }];
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };

        assert_eq!(
            latest_operation_description(&loader, Path::new(".")),
            Some("undo operation 4a3b2c1d9e0f…".to_owned())
        );
    }

    impl JjBackend for MockJjBackend {
        fn snapshot_working_copy(&self, _repo: &Path) -> Result<()> {
            *self.snapshot_calls.borrow_mut() += 1;
            Ok(())
        }

        fn diff(&self, _repo: &Path, target: &ReviewTarget) -> Result<String> {
            self.calls.borrow_mut().push(target.clone());
            if !self.diff_queue.borrow().is_empty() {
                return match self.diff_queue.borrow_mut().remove(0) {
                    Ok(diff_text) => Ok(diff_text),
                    Err(error) => bail!(error),
                };
            }
            match &self.diff_text {
                Ok(diff_text) => Ok(diff_text.clone()),
                Err(error) => bail!(error.clone()),
            }
        }

        fn change_summaries(&self, _repo: &Path) -> Result<Vec<JjChangeSummary>> {
            Ok(self.summaries.clone())
        }

        fn stack_changes(
            &self,
            _repo: &Path,
            _target: &ReviewTarget,
        ) -> Result<Vec<JjChangeSummary>> {
            Ok(self.stack.clone())
        }

        fn change_fingerprint(&self, _repo: &Path, _target: &ReviewTarget) -> Result<String> {
            match &*self.fingerprint.borrow() {
                Ok(fingerprint) => Ok(fingerprint.clone()),
                Err(error) => bail!(error.clone()),
            }
        }

        fn operations(&self, _repo: &Path) -> Result<Vec<crate::jj::JjOperationSummary>> {
            Ok(self.operations.clone())
        }

        fn diff_at_operation(
            &self,
            _repo: &Path,
            _target: &ReviewTarget,
            _operation_id: &str,
        ) -> Result<String> {
            match &self.diff_at_op {
                Some(diff) => Ok(diff.clone()),
                None => bail!("no at-op diff configured"),
            }
        }

        fn run_command(&self, _repo: &Path, args: &[String]) -> Result<String> {
            self.commands.borrow_mut().push(args.to_vec());
            match &self.command_result {
                Ok(output) => Ok(output.clone()),
                Err(error) => bail!(error.clone()),
            }
        }

        fn file_contents(&self, _repo: &Path, _rev: &str, _path: &str) -> Result<String> {
            match &self.file_contents {
                Some(contents) => Ok(contents.clone()),
                None => bail!("no file contents configured"),
            }
        }
    }

    #[test]
    fn normal_load_and_live_refresh_reload_stream_chapter_metadata() {
        let first = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let second = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n-old\n+new\n+extra\n keep\n";
        let mut backend = MockJjBackend::with_diff(Ok(first.into()));
        backend.stack = vec![JjChangeSummary {
            change_id: "abc123".into(),
            bookmarks: "feature/chapters".into(),
            description: "feat: chapter metadata".into(),
        }];
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut session = snapshot_session("");
        loader
            .load(&mut session, ReviewTarget::new("trunk()", "@"))
            .unwrap();
        assert_eq!(session.stack_changes, backend.stack);
        assert_eq!(session.change_diffs.len(), 1);
        assert_eq!(
            session.change_diffs[0]
                .1
                .files
                .iter()
                .map(|file| file.additions)
                .sum::<usize>(),
            1
        );

        backend
            .diff_queue
            .borrow_mut()
            .extend([Ok(second.into()), Ok(second.into())]);
        loader
            .load_in_place(&mut session, ReviewTarget::new("trunk()", "@"))
            .unwrap();
        assert_eq!(
            session.stack_changes[0].description,
            "feat: chapter metadata"
        );
        assert_eq!(session.stack_changes[0].bookmarks, "feature/chapters");
        assert_eq!(
            session.change_diffs[0]
                .1
                .files
                .iter()
                .map(|file| file.additions)
                .sum::<usize>(),
            2
        );
    }

    #[test]
    fn comment_capture_uses_loaded_diff_without_backend_queries_or_mutations() {
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new",
        );
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let mut mode = Mode::CommentInput {
            editor: CommentEditor::new("observed".into()),
            target: CommentInputTarget::New,
        };

        handle_key_event(
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
            &mut session,
            &mut mode,
            &keymap,
            &loader,
            &mut TuiState::default(),
        )
        .unwrap();

        assert!(matches!(mode, Mode::Normal));
        assert!(session.comments[0].observation.is_some());
        assert_eq!(*backend.snapshot_calls.borrow(), 0);
        assert!(backend.calls.borrow().is_empty());
        assert!(backend.commands.borrow().is_empty());
    }

    #[test]
    fn autosave_writes_state_changes_and_skips_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join(".gander").join("state.json");
        let mut session = snapshot_session(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        );
        let mut tui_state = TuiState {
            last_autosave_generation: Some(session.durable_state_generation()),
            ..TuiState::default()
        };

        // No changes yet: nothing should be written.
        autosave_state(&mut session, &state_path, &mut tui_state);
        assert!(!state_path.exists());

        session.toggle_viewed();
        session.add_comment("note".into());
        autosave_state(&mut session, &state_path, &mut tui_state);

        let saved = crate::state::ReviewState::load_or_default(&state_path).unwrap();
        assert!(saved.files["a.txt"].viewed);
        assert_eq!(saved.comments.len(), 1);
        assert!(tui_state.notice.is_none());

        // Unchanged session: the generation gate short-circuits the write.
        let modified_before = std::fs::metadata(&state_path).unwrap().modified().unwrap();
        autosave_state(&mut session, &state_path, &mut tui_state);
        let modified_after = std::fs::metadata(&state_path).unwrap().modified().unwrap();
        assert_eq!(modified_before, modified_after);
    }

    /// N key events over an unchanged session must perform zero
    /// session-scale work: no stream-projection rebuilds, no durable-state
    /// snapshots (autosave serialization), no filesystem canonicalization of
    /// the repo identity, and no line-fingerprint re-derivation. Guards the
    /// generation-counter cache discipline against reintroducing
    /// O(session)-per-keystroke regressions.
    #[test]
    fn key_storm_over_unchanged_session_does_no_session_scale_work() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,5 +1,5 @@\n one\n two\n-old\n+new\n four\n five\n";
        let mut session = attention_session(raw, "a.rs", Salience::Supporting);
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let mut mode = Mode::Normal;
        let mut tui_state = TuiState {
            last_autosave_generation: Some(session.durable_state_generation()),
            terminal_size: ratatui::prelude::Size::new(220, 60),
            ..TuiState::default()
        };
        let inner = Rect::new(0, 0, 220, 58);
        let storm = ['j', 'j', 'j', 'k'];
        let press =
            |key: char, session: &mut ReviewSession, mode: &mut Mode, tui_state: &mut TuiState| {
                handle_key_event(
                    KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                    session,
                    mode,
                    &keymap,
                    &loader,
                    tui_state,
                )
                .unwrap();
                // What a draw would consume: the stream projection plus the
                // measured viewport layout (annotation scope, cards, geometry).
                let _ = session.review_stream_rows();
                let _ = tui_state.diff_viewport.measure(
                    session,
                    inner,
                    render::diff_split_is_active(session, inner),
                );
                autosave_state(session, &state_path, tui_state);
            };

        // Warm every cache with one full round before sampling counters.
        for key in storm {
            press(key, &mut session, &mut mode, &mut tui_state);
        }

        let canonical_resolutions = crate::review::canonical_repo_resolutions();
        let snapshots = session.state_snapshot_count();
        let builds = session.stream_projection_build_count();
        let fingerprints = crate::anchor::line_fingerprint_derivations();

        for _ in 0..20 {
            for key in storm {
                press(key, &mut session, &mut mode, &mut tui_state);
            }
        }

        assert_eq!(
            crate::review::canonical_repo_resolutions(),
            canonical_resolutions,
            "key events must not canonicalize the repo path (fs access per keystroke)"
        );
        assert_eq!(
            session.state_snapshot_count(),
            snapshots,
            "key events over an unchanged session must not snapshot durable state"
        );
        assert_eq!(
            session.stream_projection_build_count(),
            builds,
            "key events over an unchanged session must not rebuild the stream projection"
        );
        assert_eq!(
            crate::anchor::line_fingerprint_derivations(),
            fingerprints,
            "key events must not re-derive line fingerprints"
        );
        assert!(
            !state_path.exists(),
            "an unchanged session must not autosave"
        );
    }

    #[test]
    fn autosave_generation_tracks_all_mutable_durable_session_fields() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let mut session = attention_session(raw, "a.rs", Salience::Skim);
        let mut previous = session.durable_state_generation();
        session.durable_sessions_mut()[0].title = Some("Review title".into());
        let next = session.durable_state_generation();
        assert_ne!(previous, next);
        previous = next;

        session.durable_sessions_mut()[0]
            .walkthroughs
            .push(crate::state::Walkthrough {
                id: "walk".into(),
                ..Default::default()
            });
        let next = session.durable_state_generation();
        assert_ne!(previous, next);
        previous = next;

        session.durable_sessions_mut()[0]
            .action_items
            .push(ActionItem {
                id: "item".into(),
                title: "Check behavior".into(),
                ..Default::default()
            });
        let next = session.durable_state_generation();
        assert_ne!(previous, next);
        previous = next;

        session.durable_sessions_mut()[0].disposition =
            Some(crate::state::ReviewDisposition::Approve);
        let next = session.durable_state_generation();
        assert_ne!(previous, next);
    }

    #[test]
    fn attention_only_mutations_autosave_and_quit_flushes_latest_session() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,5 +1,5 @@\n one\n two\n-old\n+new\n four\n five\n";
        let mut session = snapshot_session(raw);
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let skim = crate::attention::target_for_diff(&files, "a.rs", Some(1), None).unwrap();
        let spotlight = crate::attention::target_for_diff(&files, "a.rs", Some(4), None).unwrap();
        let mut durable = crate::state::ReviewSession {
            id: "autosave-attention".into(),
            attention_regions: vec![
                AttentionRegion {
                    target: skim,
                    salience: Salience::Skim,
                    rationale: Some("skip setup".into()),
                    source: SalienceSource::Human,
                },
                AttentionRegion {
                    target: spotlight.clone(),
                    salience: Salience::Spotlight,
                    rationale: Some("read contract".into()),
                    source: SalienceSource::Agent,
                },
            ],
            walkthroughs: vec![crate::state::Walkthrough {
                id: "walk".into(),
                steps: vec![WalkthroughStep {
                    id: "spot".into(),
                    target: spotlight,
                    title: Some("Contract".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        durable.target.base = Some(session.target.base.clone());
        durable.target.revision = Some(session.target.rev.clone());
        durable.target.repo = Some(session.canonical_repo().to_owned());
        session.durable_sessions_mut().push(durable);
        session.stream_mode = true;
        session.focus = Focus::Diff;
        let baseline = session.to_state();
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(100, 24),
            last_autosave_generation: Some(session.durable_state_generation()),
            ..TuiState::default()
        };

        let fold = session
            .review_stream()
            .rows
            .iter()
            .position(|row| matches!(row.kind, crate::app::StreamRowKind::SkimFold(_)))
            .unwrap();
        session.select_stream_row(fold, false);
        assert_eq!(
            session.acknowledge_selected_skim_fold(),
            SkimAcknowledgeResult::Acknowledged
        );
        autosave_state(&mut session, &state_path, &mut tui_state);
        let saved = ReviewState::load_or_default(&state_path).unwrap();
        assert_eq!(saved.files, baseline.files);
        assert_eq!(saved.comments, baseline.comments);
        assert_eq!(
            saved.sessions[0].attention_progress[0].kind,
            crate::state::AttentionProgressKind::SkimAcknowledged
        );

        assert!(session.jump_spotlight_with_identity(1).is_some());
        autosave_state(&mut session, &state_path, &mut tui_state);
        let saved = ReviewState::load_or_default(&state_path).unwrap();
        assert!(saved.sessions[0].attention_progress.iter().any(|progress| {
            progress.kind == crate::state::AttentionProgressKind::SpotlightVisited
        }));

        assert!(session.change_selected_salience(true));
        autosave_state(&mut session, &state_path, &mut tui_state);
        let saved = ReviewState::load_or_default(&state_path).unwrap();
        assert!(saved.sessions[0].attention_regions.iter().any(|region| {
            region.source == SalienceSource::Human && region.salience == Salience::Spotlight
        }));

        assert!(session.change_selected_salience(false));
        autosave_state(&mut session, &state_path, &mut tui_state);
        let saved = ReviewState::load_or_default(&state_path).unwrap();
        assert!(saved.sessions[0].attention_regions.iter().any(|region| {
            region.source == SalienceSource::Human && region.salience == Salience::Supporting
        }));

        // The final promote is intentionally persisted only by the same flush
        // path the run loop uses after a quit event.
        assert!(session.change_selected_salience(true));
        tui_state.enter_attention_focus(&mut session);
        finish_ephemeral_views_on_quit(&mut session, &mut tui_state);
        autosave_state(&mut session, &state_path, &mut tui_state);
        let saved = ReviewState::load_or_default(&state_path).unwrap();
        assert!(saved.sessions[0].attention_regions.iter().any(|region| {
            region.source == SalienceSource::Human && region.salience == Salience::Spotlight
        }));
        assert!(tui_state.attention_focus.is_none());
    }

    #[test]
    fn autosave_merges_unseen_disk_state_from_other_targets() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut on_disk = crate::state::ReviewState::default();
        on_disk.files.insert(
            "b.txt".to_owned(),
            crate::state::FileState {
                fingerprint: "b-fp".to_owned(),
                viewed: true,
                ..Default::default()
            },
        );
        on_disk.comments.push(Comment {
            id: "b-comment".to_owned(),
            path: Some("b.txt".to_owned()),
            line: Some(1),
            end_line: None,
            anchor: None,
            body: "from another target".to_owned(),
            kind: None,
            action: None,
            state: CommentState::Draft,
            created_at: chrono::Utc::now(),
            ..Default::default()
        });
        on_disk.save(&state_path).unwrap();

        let mut session = snapshot_session(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        );
        let mut tui_state = TuiState {
            last_autosave_generation: Some(session.durable_state_generation()),
            ..TuiState::default()
        };
        session.toggle_viewed();
        session.add_comment("from current target".into());

        autosave_state(&mut session, &state_path, &mut tui_state);

        let saved = crate::state::ReviewState::load_or_default(&state_path).unwrap();
        assert!(saved.files["a.txt"].viewed);
        assert_eq!(saved.files["b.txt"].fingerprint, "b-fp");
        assert!(
            saved
                .comments
                .iter()
                .any(|comment| comment.id == "b-comment")
        );
        assert!(
            saved
                .comments
                .iter()
                .any(|comment| comment.path.as_deref() == Some("a.txt"))
        );
    }

    #[test]
    fn review_loader_uses_injected_jj_backend() {
        let mut session = snapshot_session(
            r#"diff --git a/old.rs b/old.rs
--- a/old.rs
+++ b/old.rs
@@ -1 +1 @@
-old
+old2
"#,
        );
        let backend = MockJjBackend::with_diff(Ok(r#"diff --git a/new.rs b/new.rs
--- a/new.rs
+++ b/new.rs
@@ -1 +1 @@
-old
+new
"#
        .to_owned()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };

        loader
            .load(&mut session, ReviewTarget::parent_to_current())
            .unwrap();

        assert_eq!(
            backend.calls.borrow().as_slice(),
            [ReviewTarget::parent_to_current()]
        );
        assert_eq!(session.selected_file().unwrap().path, "new.rs");
    }

    #[test]
    fn compare_errors_become_tui_notices() {
        let mut session = snapshot_session("");
        let backend = MockJjBackend::with_diff(Err("boom".to_owned()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();

        load_review_target(
            &loader,
            &mut session,
            ReviewTarget::trunk_to_current(),
            &mut tui_state,
        );

        let notice = tui_state.notice.unwrap();
        assert_eq!(notice.level, UiNoticeLevel::Error);
        assert!(notice.message.contains("failed to load trunk()..@"));
        assert!(notice.message.contains("boom"));
    }

    #[test]
    fn comment_editor_can_open_reflow_and_update_an_existing_long_comment() {
        let mut session = snapshot_session(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        );
        let original = "first long line with 界 and e\u{301}\nsecond line\ntrailing 👩🏽‍💻";
        session.add_comment(original.into());
        let id = session.comments[0].id.clone();
        session.move_to_comment(1);
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut mode = Mode::Normal;
        let mut tui_state = TuiState::default();

        assert!(
            !handle_normal_action(
                Action::EditComment,
                &mut session,
                &mut mode,
                &loader,
                &mut tui_state,
            )
            .unwrap()
        );
        let Mode::CommentInput { editor, target } = &mut mode else {
            panic!("expected existing comment editor");
        };
        assert_eq!(target, &CommentInputTarget::Edit { id: id.clone() });
        assert_eq!(editor.text, original);
        editor.resize(8, 3);
        assert!(editor.visible_scroll(8, 3) > 0);
        editor.insert_char('!');

        assert!(handle_comment_action(
            Action::SubmitComment,
            &mut session,
            editor,
            target,
            &mut tui_state,
        ));

        assert_eq!(session.comments.len(), 1);
        assert_eq!(session.comments[0].body, format!("{original}!"));
    }

    #[test]
    fn revset_input_loads_typed_target() {
        let mut session = snapshot_session("");
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let mut input = RevsetInputState::new("", "");
        for ch in "ancestors(@, 2)".chars() {
            assert!(!handle_revset_input_key(
                KeyEvent::from(KeyCode::Char(ch)),
                &mut input,
                &mut session,
                &keymap,
                &loader,
                &mut tui_state,
            ));
        }
        input.toggle_field();
        for ch in "@".chars() {
            handle_revset_input_key(
                KeyEvent::from(KeyCode::Char(ch)),
                &mut input,
                &mut session,
                &keymap,
                &loader,
                &mut tui_state,
            );
        }

        assert!(handle_revset_input_key(
            KeyEvent::from(KeyCode::Enter),
            &mut input,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));

        assert_eq!(
            backend.calls.borrow().as_slice(),
            [ReviewTarget::new("ancestors(@, 2)", "@")]
        );
        assert_eq!(session.target, ReviewTarget::new("ancestors(@, 2)", "@"));
    }

    #[test]
    fn revset_input_requires_both_fields_before_loading() {
        let mut session = snapshot_session("");
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let mut input = RevsetInputState::new("", "@");

        assert!(!handle_revset_input_key(
            KeyEvent::from(KeyCode::Enter),
            &mut input,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));

        assert!(backend.calls.borrow().is_empty());
        let notice = tui_state.notice.unwrap();
        assert_eq!(notice.level, UiNoticeLevel::Info);
        assert!(notice.message.contains("required"));
    }

    #[test]
    fn text_filter_accepts_literal_j_k_and_uses_safe_movement_keys() {
        let mut session = snapshot_session(
            "diff --git a/jk.txt b/jk.txt\n--- a/jk.txt\n+++ b/jk.txt\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let mut search = FileSearchState::new(&session);
        let mut tui_state = TuiState::default();

        assert!(!handle_file_search_key(
            KeyEvent::from(KeyCode::Char('j')),
            &mut search,
            &mut session,
            &keymap,
            &mut tui_state,
        ));
        assert!(!handle_file_search_key(
            KeyEvent::from(KeyCode::Char('k')),
            &mut search,
            &mut session,
            &keymap,
            &mut tui_state,
        ));
        assert_eq!(search.query, "jk");

        search.query.clear();
        search.filtered = (0..search.files.len()).collect();
        assert!(!handle_file_search_key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
            &mut search,
            &mut session,
            &keymap,
            &mut tui_state,
        ));
        assert_eq!(search.query, "");
    }

    fn stack_change(id: &str, description: &str) -> JjChangeSummary {
        JjChangeSummary {
            change_id: id.to_owned(),
            bookmarks: String::new(),
            description: description.to_owned(),
        }
    }

    #[test]
    fn stack_previous_from_working_copy_steps_to_prior_change() {
        let mut session = snapshot_session("");
        let mut backend = MockJjBackend::with_diff(Ok(String::new()));
        backend.stack = vec![
            stack_change("aaa", "feat: first"),
            stack_change("bbb", "feat: second"),
            stack_change("ccc", "feat: third"),
        ];
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        session.target = ReviewTarget::new("trunk()", "@");

        step_stack(&loader, &mut session, -1, &mut tui_state);

        assert_eq!(session.target, ReviewTarget::new("bbb-", "bbb"));
        let notice = tui_state.notice.unwrap();
        assert_eq!(notice.message, "stack 2/3: feat: second");
    }

    #[test]
    fn stack_step_excludes_base_change_from_positions() {
        let mut session = snapshot_session("");
        let mut backend = MockJjBackend::with_diff(Ok(String::new()));
        backend.stack = vec![
            JjChangeSummary {
                change_id: "base".to_owned(),
                bookmarks: "main".to_owned(),
                description: "base".to_owned(),
            },
            stack_change("aaa", "feat: first"),
            stack_change("bbb", "feat: second"),
        ];
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        session.target = ReviewTarget::new("main", "aaa");

        step_stack(&loader, &mut session, -1, &mut tui_state);

        assert_eq!(session.target, ReviewTarget::new("main", "aaa"));
        assert!(
            tui_state
                .notice
                .unwrap()
                .message
                .contains("already at the bottom")
        );
    }

    #[test]
    fn autosave_merges_external_write_made_after_tui_load() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut initial = crate::state::ReviewState::default();
        initial.comments.push(Comment {
            id: "initial".to_owned(),
            path: Some("a.txt".to_owned()),
            line: Some(1),
            end_line: None,
            anchor: None,
            body: "initial".to_owned(),
            kind: None,
            action: None,
            state: CommentState::Draft,
            created_at: chrono::Utc::now(),
            ..Default::default()
        });
        initial.save(&state_path).unwrap();

        let mut session = snapshot_session(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        );
        session
            .apply_review_state(crate::state::ReviewState::load_or_default(&state_path).unwrap());
        let mut tui_state = TuiState {
            state_mtime: state_file_mtime(&state_path),
            last_autosave_generation: Some(session.durable_state_generation()),
            ..TuiState::default()
        };

        let mut external = crate::state::ReviewState::load_or_default(&state_path).unwrap();
        external.comments.push(Comment {
            id: "external".to_owned(),
            path: Some("a.txt".to_owned()),
            line: Some(2),
            end_line: None,
            anchor: None,
            body: "probe B".to_owned(),
            kind: None,
            action: None,
            state: CommentState::Draft,
            created_at: chrono::Utc::now(),
            ..Default::default()
        });
        external.save(&state_path).unwrap();
        session.toggle_viewed();

        autosave_state(&mut session, &state_path, &mut tui_state);

        let saved = crate::state::ReviewState::load_or_default(&state_path).unwrap();
        assert!(saved.comments.iter().any(|comment| comment.id == "initial"));
        assert!(
            saved
                .comments
                .iter()
                .any(|comment| comment.id == "external")
        );
        assert!(
            session
                .comments
                .iter()
                .any(|comment| comment.id == "external")
        );
    }

    #[test]
    fn compare_launch_target_restores_custom_launch_after_stack_step() {
        let mut session = snapshot_session("diff --git a/main.rs b/main.rs\n");
        session.target = ReviewTarget::new("main", "@");
        let mut backend = MockJjBackend::with_diff(Ok(String::new()));
        backend.stack = vec![stack_change("aaa", "feat: first"), stack_change("bbb", "")];
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState {
            launch_target: Some(session.target.clone()),
            ..TuiState::default()
        };
        let mut mode = Mode::Normal;

        step_stack(&loader, &mut session, -1, &mut tui_state);
        assert_ne!(session.target, ReviewTarget::new("main", "@"));

        handle_normal_action(
            Action::CompareTrunk,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();

        assert_eq!(session.target, ReviewTarget::new("main", "@"));
        assert!(
            backend
                .calls
                .borrow()
                .contains(&ReviewTarget::new("main", "@"))
        );
    }

    #[test]
    fn stack_next_moves_forward_and_stops_at_top() {
        let mut session = snapshot_session("");
        let mut backend = MockJjBackend::with_diff(Ok(String::new()));
        backend.stack = vec![stack_change("aaa", "feat: first"), stack_change("bbb", "")];
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        session.target = ReviewTarget::new("aaa-", "aaa");

        step_stack(&loader, &mut session, 1, &mut tui_state);
        assert_eq!(session.target, ReviewTarget::new("bbb-", "bbb"));
        assert_eq!(
            tui_state.notice.as_ref().unwrap().message,
            "stack 2/2: (no description)"
        );

        step_stack(&loader, &mut session, 1, &mut tui_state);
        assert_eq!(session.target, ReviewTarget::new("bbb-", "bbb"));
        assert!(
            tui_state
                .notice
                .unwrap()
                .message
                .contains("already at the top")
        );
    }

    #[test]
    fn stack_step_with_empty_stack_shows_notice() {
        let mut session = snapshot_session("");
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();

        step_stack(&loader, &mut session, 1, &mut tui_state);

        assert!(backend.calls.borrow().is_empty());
        assert!(
            tui_state
                .notice
                .unwrap()
                .message
                .contains("no stack changes")
        );
    }

    #[test]
    fn operation_picker_enter_applies_incremental_review() {
        let current_diff = r#"diff --git a/same.rs b/same.rs
--- a/same.rs
+++ b/same.rs
@@ -1 +1 @@
-old
+new
diff --git a/changed.rs b/changed.rs
--- a/changed.rs
+++ b/changed.rs
@@ -1 +1 @@
-old
+other
"#;
        let mut session = snapshot_session(current_diff);
        let same_file_diff = session
            .files
            .iter()
            .find(|file| file.path == "same.rs")
            .unwrap()
            .diff
            .raw
            .clone();
        let mut backend = MockJjBackend::with_diff(Ok(current_diff.to_owned()));
        backend.diff_at_op = Some(format!("{same_file_diff}\n"));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let mut picker = OperationPickerState::new(vec![crate::jj::JjOperationSummary {
            operation_id: "op123".to_owned(),
            time: "1 hour ago".to_owned(),
            description: "snapshot".to_owned(),
        }]);
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert!(handle_operation_picker_key(
            KeyEvent::from(KeyCode::Enter),
            &mut picker,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));

        let same = session
            .files
            .iter()
            .find(|file| file.path == "same.rs")
            .unwrap();
        let changed = session
            .files
            .iter()
            .find(|file| file.path == "changed.rs")
            .unwrap();
        assert!(!same.viewed);
        assert!(same.caught_up);
        assert!(!changed.viewed);
        assert!(!changed.caught_up);
        let notice = tui_state.notice.unwrap();
        assert!(
            notice
                .message
                .contains("1 file(s) caught up (unchanged since op123), 0 already viewed")
        );
        assert!(notice.message.contains("1 changed/new"));
    }

    #[test]
    fn operation_picker_uses_standard_movement_bindings_and_fallbacks() {
        let mut session = snapshot_session("");
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let config = KeybindingsConfig {
            popup_move_down: vec!["alt-n".to_owned()],
            popup_move_up: vec!["alt-e".to_owned()],
            ..Default::default()
        };
        let keymap = KeyMap::try_from(&config).unwrap();
        let op = |id: &str| crate::jj::JjOperationSummary {
            operation_id: id.to_owned(),
            time: String::new(),
            description: String::new(),
        };
        let mut picker = OperationPickerState::new(vec![op("one"), op("two"), op("three")]);

        assert!(!handle_operation_picker_key(
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT),
            &mut picker,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));
        assert_eq!(picker.selected, 1);

        assert!(!handle_operation_picker_key(
            KeyEvent::from(KeyCode::Char('j')),
            &mut picker,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));
        assert_eq!(picker.selected, 2);

        assert!(!handle_operation_picker_key(
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT),
            &mut picker,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));
        assert_eq!(picker.selected, 1);

        assert!(!handle_operation_picker_key(
            KeyEvent::from(KeyCode::Up),
            &mut picker,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn operation_picker_preview_uses_incremental_review_counts() {
        let current_diff = r#"diff --git a/same.rs b/same.rs
--- a/same.rs
+++ b/same.rs
@@ -1 +1 @@
-old
+new
diff --git a/viewed.rs b/viewed.rs
--- a/viewed.rs
+++ b/viewed.rs
@@ -1 +1 @@
-old
+new
diff --git a/changed.rs b/changed.rs
--- a/changed.rs
+++ b/changed.rs
@@ -1 +1 @@
-old
+other
"#;
        let mut session = snapshot_session(current_diff);
        session
            .files
            .iter_mut()
            .find(|file| file.path == "viewed.rs")
            .unwrap()
            .viewed = true;
        let same_file_diff = session
            .files
            .iter()
            .find(|file| file.path == "same.rs")
            .unwrap()
            .diff
            .raw
            .clone();
        let viewed_file_diff = session
            .files
            .iter()
            .find(|file| file.path == "viewed.rs")
            .unwrap()
            .diff
            .raw
            .clone();
        let mut backend = MockJjBackend::with_diff(Ok(current_diff.to_owned()));
        backend.diff_at_op = Some(format!("{same_file_diff}\n{viewed_file_diff}\n"));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut picker = OperationPickerState::new(vec![crate::jj::JjOperationSummary {
            operation_id: "op123".to_owned(),
            time: String::new(),
            description: String::new(),
        }]);

        refresh_operation_picker_preview(&mut picker, &session, &loader);

        assert_eq!(
            picker.preview.as_deref(),
            Some("will mark 1 caught up · 1 already viewed · 1 need re-review")
        );
    }

    #[test]
    fn operation_compare_refreshes_current_diff_before_marking_viewed() {
        let initial = r#"diff --git a/file.rs b/file.rs
--- a/file.rs
+++ b/file.rs
@@ -1 +1 @@
-old
+new
"#;
        let refreshed = r#"diff --git a/file.rs b/file.rs
--- a/file.rs
+++ b/file.rs
@@ -1 +1 @@
-old
+newer
"#;
        let mut session = snapshot_session(initial);
        let mut backend = MockJjBackend::with_diff(Ok(initial.to_owned()));
        backend
            .diff_queue
            .borrow_mut()
            .push(Ok(refreshed.to_owned()));
        backend.diff_at_op = Some(initial.to_owned());
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let operation = crate::jj::JjOperationSummary {
            operation_id: "op123".to_owned(),
            time: String::new(),
            description: String::new(),
        };

        apply_incremental_review(&loader, &mut session, &operation, &mut tui_state);

        assert!(!session.files[0].viewed);
        assert!(!session.files[0].caught_up);
        assert_eq!(session.files[0].diff.raw, refreshed.trim_end());
        assert!(tui_state.notice.unwrap().message.contains(
            "0 file(s) caught up (unchanged since op123), 0 already viewed; 1 changed/new"
        ));
    }

    #[test]
    fn operation_picker_errors_become_notices() {
        let mut session = snapshot_session("");
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let mut picker = OperationPickerState::new(vec![crate::jj::JjOperationSummary {
            operation_id: "op123".to_owned(),
            time: String::new(),
            description: String::new(),
        }]);
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert!(handle_operation_picker_key(
            KeyEvent::from(KeyCode::Enter),
            &mut picker,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));

        let notice = tui_state.notice.unwrap();
        assert_eq!(notice.level, UiNoticeLevel::Error);
        assert!(notice.message.contains("op123"));
    }

    #[test]
    fn jj_helper_runs_only_after_explicit_confirmation() {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1 +1 @@
-old
+new
"#,
        );
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let mut state = JjHelperState::for_session(&session);
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        // First Enter only advances to the confirmation step.
        assert!(!handle_jj_helpers_key(
            KeyEvent::from(KeyCode::Enter),
            &mut state,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));
        assert!(state.confirming);
        assert!(backend.commands.borrow().is_empty());

        // Second Enter actually runs the command and closes the popup.
        assert!(handle_jj_helpers_key(
            KeyEvent::from(KeyCode::Enter),
            &mut state,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));
        assert_eq!(
            backend.commands.borrow().as_slice(),
            [vec!["squash".to_owned(), "-r".to_owned(), "@".to_owned()]]
        );
        assert!(
            tui_state
                .notice
                .unwrap()
                .message
                .contains("ran jj squash -r @")
        );
    }

    #[test]
    fn jj_helper_confirmation_accepts_only_the_literal_enter_key() {
        // Bind popup-select to a payload-alphabet key ("g", part of the OSC
        // 11 reply alphabet). Selection gestures stay remappable, but the
        // final verbatim-command confirmation must ignore the custom binding
        // and fire only on the immutable Enter key (docs/theme.md): an OSC
        // payload can never contain Enter, so no leak reaches the shell-out
        // under any keybinding configuration.
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1 +1 @@
-old
+new
"#,
        );
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let mut state = JjHelperState::for_session(&session);
        let config = KeybindingsConfig {
            popup_select: vec!["g".to_owned()],
            ..KeybindingsConfig::default()
        };
        let keymap = KeyMap::try_from(&config).unwrap();

        // The custom select key advances to the confirmation step.
        assert!(!handle_jj_helpers_key(
            KeyEvent::from(KeyCode::Char('g')),
            &mut state,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));
        assert!(state.confirming);

        // The custom select key must NOT confirm the verbatim command.
        assert!(!handle_jj_helpers_key(
            KeyEvent::from(KeyCode::Char('g')),
            &mut state,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));
        assert!(state.confirming);
        assert!(backend.commands.borrow().is_empty());

        // Only the literal Enter key runs it.
        assert!(handle_jj_helpers_key(
            KeyEvent::from(KeyCode::Enter),
            &mut state,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));
        assert_eq!(
            backend.commands.borrow().as_slice(),
            [vec!["squash".to_owned(), "-r".to_owned(), "@".to_owned()]]
        );
    }

    #[test]
    fn jj_helper_esc_backs_out_of_confirmation_without_running() {
        let mut session = snapshot_session("");
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let mut state = JjHelperState::for_session(&session);
        state.confirming = true;
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        // Esc dismisses the confirmation layer but keeps the popup open.
        assert!(!handle_jj_helpers_key(
            KeyEvent::from(KeyCode::Esc),
            &mut state,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));
        assert!(!state.confirming);

        // A second Esc closes the popup entirely; nothing ever ran.
        assert!(handle_jj_helpers_key(
            KeyEvent::from(KeyCode::Esc),
            &mut state,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));
        assert!(backend.commands.borrow().is_empty());
    }

    #[test]
    fn jj_helper_failures_become_error_notices() {
        let mut session = snapshot_session("");
        let mut backend = MockJjBackend::with_diff(Ok(String::new()));
        backend.command_result = Err("immutable commit".to_owned());
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let mut state = JjHelperState::for_session(&session);
        state.confirming = true;
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert!(handle_jj_helpers_key(
            KeyEvent::from(KeyCode::Enter),
            &mut state,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));

        let notice = tui_state.notice.unwrap();
        assert_eq!(notice.level, UiNoticeLevel::Error);
        assert!(notice.message.contains("jj squash -r @ failed"));
        assert!(notice.message.contains("immutable commit"));
    }

    #[test]
    fn overlay_polling_applies_agent_ordering_on_mtime_change() {
        let dir = tempfile::tempdir().unwrap();
        let overlay_path = dir.path().join("agent.json");
        let mut session = snapshot_session(
            r#"diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1 +1 @@
-old
+new
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1 +1 @@
-old
+new
"#,
        );
        let mut tui_state = TuiState::default();
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };

        // No overlay on disk yet: nothing happens.
        maybe_reload_agent_overlay(&mut session, &overlay_path, &mut tui_state, &loader, true);
        assert!(session.agent_ordering.is_empty());
        assert!(tui_state.notice.is_none());

        crate::agent::AgentOverlay {
            ordering: vec!["b.rs".to_owned()],
            ..Default::default()
        }
        .save(&overlay_path)
        .unwrap();

        maybe_reload_agent_overlay(&mut session, &overlay_path, &mut tui_state, &loader, true);
        assert_eq!(session.agent_ordering, ["b.rs"]);
        assert_eq!(
            tui_state.notice.as_ref().unwrap().message,
            "agent ordering/flags updated"
        );

        // Unchanged mtime: no re-notification.
        tui_state.notice = None;
        maybe_reload_agent_overlay(&mut session, &overlay_path, &mut tui_state, &loader, true);
        assert!(tui_state.notice.is_none());
    }

    fn draft_session_with_overlay(dir: &std::path::Path) -> (ReviewSession, PathBuf) {
        let mut session = snapshot_session(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        );
        let overlay_path = dir.join("agent.json");
        let draft = session
            .add_agent_draft(
                "a.txt".to_owned(),
                Some(1),
                "agent thinks this is wrong".to_owned(),
            )
            .unwrap();
        session
            .comments
            .iter_mut()
            .find(|comment| comment.id == draft.id)
            .unwrap()
            .id = "draft-1".to_owned();
        (session, overlay_path)
    }

    #[test]
    fn accepting_a_draft_promotes_the_durable_comment() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, overlay_path) = draft_session_with_overlay(dir.path());
        let mut tui_state = TuiState {
            agent_overlay_path: Some(overlay_path.clone()),
            ..TuiState::default()
        };
        let mut list = DraftListState::new(&session);
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        let next = handle_draft_list_key(
            KeyEvent::from(KeyCode::Enter),
            &mut list,
            &mut session,
            &keymap,
            &mut tui_state,
        );

        // Last pending draft handled: popup closes.
        assert!(matches!(next, Some(Mode::Normal)));
        assert_eq!(session.comments.len(), 1);
        assert_eq!(session.comments[0].body, "agent thinks this is wrong");
        assert_eq!(session.comments[0].line, Some(1));
        assert_eq!(session.comments[0].state, crate::state::CommentState::Todo);
        assert_eq!(
            session.comments[0].channel,
            crate::state::Channel::Delegation
        );
        assert_eq!(session.comments[0].id, "draft-1");
    }

    #[test]
    fn accepting_onboarding_draft_honors_fixed_private_channel_without_todo_publishability() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, overlay_path) = draft_session_with_overlay(dir.path());
        session.comment_default_channel = Some(Channel::Note);
        let mut tui_state = TuiState {
            agent_overlay_path: Some(overlay_path),
            ..TuiState::default()
        };

        assert!(accept_agent_draft(
            &mut session,
            &mut tui_state,
            "draft-1",
            None,
        ));

        assert_eq!(session.comments[0].channel, Channel::Note);
        assert_eq!(session.comments[0].state, CommentState::Draft);
        assert!(session.pending_agent_drafts().is_empty());
    }

    #[test]
    fn accepting_agent_draft_as_collaboration_creates_team_todo() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, overlay_path) = draft_session_with_overlay(dir.path());
        let mut tui_state = TuiState {
            agent_overlay_path: Some(overlay_path),
            ..TuiState::default()
        };

        assert!(accept_agent_draft_with_channel(
            &mut session,
            &mut tui_state,
            "draft-1",
            None,
            Channel::Collaboration,
        ));
        assert_eq!(session.comments[0].channel, Channel::Collaboration);
        assert_eq!(session.comments[0].state, CommentState::Todo);
        assert!(session.pending_agent_drafts().is_empty());
    }

    #[test]
    fn accepting_agent_draft_as_onboarding_acknowledges_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, overlay_path) = draft_session_with_overlay(dir.path());
        let mut tui_state = TuiState {
            agent_overlay_path: Some(overlay_path),
            ..TuiState::default()
        };

        assert!(accept_agent_draft_with_channel(
            &mut session,
            &mut tui_state,
            "draft-1",
            None,
            Channel::Onboarding,
        ));
        assert_eq!(session.comments[0].channel, Channel::Onboarding);
        assert_eq!(session.comments[0].state, CommentState::Resolved);
        assert!(session.pending_agent_drafts().is_empty());
    }

    #[test]
    fn draft_acceptance_finishes_reflow_without_cursor_placement() {
        let long = "wrapped draft target ".repeat(30);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        let row = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == long)
            .unwrap();
        session.diff_scroll = row as u16;
        session.diff_cursor = row;
        let draft = session
            .add_agent_draft("a.txt".into(), Some(1), "draft comment".into())
            .unwrap();
        session
            .comments
            .iter_mut()
            .find(|comment| comment.id == draft.id)
            .unwrap()
            .id = "draft-detached".into();
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(30, 5),
            ..TuiState::default()
        };
        let inner = current_diff_inner(&session, &tui_state);
        tui_state
            .diff_viewport
            .visual_scroll(&mut session, inner, 2);
        let continuation = tui_state.diff_viewport.visual_state(&session).0;
        assert!(continuation > 0);

        assert!(accept_agent_draft(
            &mut session,
            &mut tui_state,
            "draft-detached",
            None,
        ));

        assert_eq!(session.diff_cursor, row);
        assert_eq!(
            tui_state.diff_viewport.visual_state(&session).0,
            continuation
        );
    }

    #[test]
    fn stale_flag_destination_does_not_reset_detached_viewport() {
        let long = "detached flag target ".repeat(30);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        session.agent_flags.push(crate::agent::AgentFlag {
            id: "stale".into(),
            path: "missing.txt".into(),
            line: Some(10),
            reason: "stale".into(),
            priority: crate::agent::FlagPriority::High,
        });
        let row = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == long)
            .unwrap();
        session.diff_scroll = row as u16;
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(30, 5),
            ..TuiState::default()
        };
        let inner = current_diff_inner(&session, &tui_state);
        tui_state
            .diff_viewport
            .visual_scroll(&mut session, inner, 2);
        let before = tui_state.diff_viewport.visual_state(&session);
        let mut list = FlagListState::new(&session);
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert!(handle_flag_list_key(
            KeyEvent::from(KeyCode::Enter),
            &mut list,
            &mut session,
            &keymap,
            &mut tui_state,
        ));
        assert_eq!(tui_state.diff_viewport.visual_state(&session), before);
    }

    #[test]
    fn no_op_explicit_navigation_keeps_detached_viewport() {
        let long = "plain text without symbols ".repeat(30);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        session.focus = Focus::Diff;
        session.diff_scroll = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == long)
            .unwrap() as u16;
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(30, 5),
            ..TuiState::default()
        };
        let inner = current_diff_inner(&session, &tui_state);
        tui_state
            .diff_viewport
            .visual_scroll(&mut session, inner, 2);
        let before = (
            session.diff_scroll,
            tui_state.diff_viewport.visual_state(&session),
        );
        let mut mode = Mode::Normal;

        handle_normal_action(
            Action::NextSymbol,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();

        assert_eq!(
            (
                session.diff_scroll,
                tui_state.diff_viewport.visual_state(&session)
            ),
            before
        );
    }

    #[test]
    fn cursor_action_measures_once_and_help_does_not_measure() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+changed\n three\n",
        );
        session.focus = Focus::Diff;
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(120, 20),
            ..TuiState::default()
        };
        let mut mode = Mode::Normal;

        handle_normal_action(
            Action::MoveDown,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert_eq!(tui_state.diff_viewport.measurement_requests(), 1);

        handle_normal_action(
            Action::Help,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert_eq!(tui_state.diff_viewport.measurement_requests(), 1);
    }

    #[test]
    fn hidden_file_viewport_survives_intervening_normal_action() {
        let long = "hidden viewport ".repeat(30);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        session.mark_all_viewed();
        session.diff_scroll = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == long)
            .unwrap() as u16;
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(30, 5),
            ..TuiState::default()
        };
        let inner = current_diff_inner(&session, &tui_state);
        tui_state
            .diff_viewport
            .visual_scroll(&mut session, inner, 2);
        let before = (
            session.diff_scroll,
            tui_state.diff_viewport.visual_state(&session),
        );
        let mut mode = Mode::Normal;

        handle_normal_action(
            Action::CycleViewedFilter,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert!(session.selected_visible_file().is_none());
        handle_normal_action(
            Action::ToggleGutterBar,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        handle_normal_action(
            Action::CycleViewedFilter,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();

        assert!(session.selected_visible_file().is_some());
        assert_eq!(
            (
                session.diff_scroll,
                tui_state.diff_viewport.visual_state(&session)
            ),
            before
        );
    }

    #[test]
    fn file_comment_navigation_restores_file_viewport_without_revealing_cursor() {
        let mut diff = String::new();
        for path in ["a.txt", "b.txt"] {
            diff.push_str(&format!(
                "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1,20 +1,20 @@\n"
            ));
            for line in 1..=20 {
                diff.push_str(&format!(" {path} line {line}\n"));
            }
        }
        let mut session = snapshot_session(&diff);
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(120, 8),
            ..TuiState::default()
        };
        let inner = current_diff_inner(&session, &tui_state);
        session.select_file_index(1);
        tui_state.diff_viewport.file_restored(&session);
        session.add_file_comment("review b as a file".into());
        tui_state
            .diff_viewport
            .visual_scroll(&mut session, inner, 10);
        let b_top = session.diff_scroll;
        assert!(b_top > 0);
        session.select_file_index(0);
        tui_state.diff_viewport.file_restored(&session);

        let mut list = CommentListState::default();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        assert!(handle_comment_list_key(
            KeyEvent::from(KeyCode::Enter),
            &mut list,
            &mut session,
            &keymap,
            &mut tui_state,
        ));

        assert_eq!(session.selected_file().unwrap().path, "b.txt");
        assert_eq!(session.focus, Focus::Files);
        assert_eq!(session.diff_scroll, b_top);
    }

    #[test]
    fn file_level_flag_uses_top_placement_without_losing_horizontal_scroll() {
        let long = "file level flag target ".repeat(30);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        session.diff_cues.soft_wrap = false;
        session.agent_flags.push(crate::agent::AgentFlag {
            id: "file".into(),
            path: "a.txt".into(),
            line: None,
            reason: "review the file".into(),
            priority: crate::agent::FlagPriority::High,
        });
        let row = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == long)
            .unwrap();
        session.diff_cursor = row;
        session.diff_scroll = row as u16;
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(30, 5),
            ..TuiState::default()
        };
        let inner = current_diff_inner(&session, &tui_state);
        tui_state
            .diff_viewport
            .horizontal_scroll(&session, inner, 7);
        let mut list = FlagListState::new(&session);
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert!(handle_flag_list_key(
            KeyEvent::from(KeyCode::Enter),
            &mut list,
            &mut session,
            &keymap,
            &mut tui_state,
        ));
        assert_eq!(session.diff_scroll, 0);
        assert_eq!(tui_state.diff_viewport.visual_state(&session), (0, 7));
    }

    #[test]
    fn autosave_external_merge_preserves_detached_viewport() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("review.json");
        let long = "autosave merge target ".repeat(30);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        let row = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == long)
            .unwrap();
        session.diff_scroll = row as u16;
        session.diff_cursor = row;
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(30, 5),
            state_mtime: Some(std::time::SystemTime::UNIX_EPOCH),
            ..TuiState::default()
        };
        let inner = current_diff_inner(&session, &tui_state);
        tui_state
            .diff_viewport
            .visual_scroll(&mut session, inner, 2);
        let continuation = tui_state.diff_viewport.visual_state(&session).0;

        let mut external = session.clone();
        external.focus = Focus::Diff;
        external.add_comment("concurrent external comment".into());
        external.to_state().save(&state_path).unwrap();
        autosave_state(&mut session, &state_path, &mut tui_state);

        assert!(
            session
                .comments
                .iter()
                .any(|comment| comment.body == "concurrent external comment")
        );
        assert_eq!(
            tui_state.diff_viewport.visual_state(&session).0,
            continuation
        );
    }

    #[test]
    fn discarding_a_draft_deletes_the_durable_comment_and_tombstones_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, overlay_path) = draft_session_with_overlay(dir.path());
        let mut tui_state = TuiState {
            agent_overlay_path: Some(overlay_path.clone()),
            ..TuiState::default()
        };
        let mut list = DraftListState::new(&session);
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        let next = handle_draft_list_key(
            KeyEvent::from(KeyCode::Char('x')),
            &mut list,
            &mut session,
            &keymap,
            &mut tui_state,
        );

        assert!(matches!(next, Some(Mode::Normal)));
        assert!(session.comments.is_empty());
        assert!(tui_state.state_tombstones.comments.contains("draft-1"));
    }

    #[test]
    fn editing_a_draft_accepts_it_with_the_edited_body() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, overlay_path) = draft_session_with_overlay(dir.path());
        let mut tui_state = TuiState {
            agent_overlay_path: Some(overlay_path.clone()),
            ..TuiState::default()
        };
        let mut list = DraftListState::new(&session);
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        let next = handle_draft_list_key(
            KeyEvent::from(KeyCode::Char('e')),
            &mut list,
            &mut session,
            &keymap,
            &mut tui_state,
        );
        let Some(Mode::CommentInput { mut editor, target }) = next else {
            panic!("expected comment input mode");
        };
        assert_eq!(editor.text, "agent thinks this is wrong");

        editor.text = "human-edited note".to_owned();
        editor.cursor = editor.text.len();
        assert!(handle_comment_action(
            Action::SubmitComment,
            &mut session,
            &mut editor,
            &target,
            &mut tui_state,
        ));

        assert_eq!(session.comments.len(), 1);
        assert_eq!(session.comments[0].body, "human-edited note");
        assert_eq!(session.comments[0].state, crate::state::CommentState::Todo);
    }

    #[test]
    fn accepting_draft_for_missing_file_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, overlay_path) = draft_session_with_overlay(dir.path());
        session.comments[0].path = Some("gone.rs".to_owned());
        let mut tui_state = TuiState {
            agent_overlay_path: Some(overlay_path.clone()),
            ..TuiState::default()
        };

        assert!(!accept_agent_draft(
            &mut session,
            &mut tui_state,
            "draft-1",
            None
        ));

        assert_eq!(session.comments.len(), 1);
        assert_eq!(session.comments[0].state, crate::state::CommentState::Draft);
        let notice = tui_state.notice.unwrap();
        assert_eq!(notice.level, UiNoticeLevel::Error);
        assert!(notice.message.contains("gone.rs"));
    }

    #[test]
    fn target_and_view_modals_block_click_wheel_and_drag_from_review() {
        let diff = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,4 +1,4 @@\n-old\n-old2\n-old3\n-old4\n+new\n+new2\n+new3\n+new4\n";
        let size = ratatui::prelude::Size::new(80, 12);
        let layout = ui_layout(
            Rect::new(0, 0, size.width, size.height),
            true,
            &UiConfig::default(),
            30,
        );
        let files = inner_bordered(layout.files);
        let diff_area = inner_bordered(layout.diff);

        for mut mode in [
            Mode::TargetChooser(TargetChooserState::new(Vec::new(), "trunk()", "@")),
            Mode::ViewOptions(ViewOptionsState::default()),
        ] {
            let mut session = snapshot_session(diff);
            session.focus = Focus::Diff;
            let before_cursor = session.diff_cursor;
            let mut tui_state = TuiState {
                diff_drag: Some(DiffDrag {
                    start_row: before_cursor,
                    start_file_index: 0,
                    current_row: before_cursor,
                    saw_drag: false,
                }),
                ..TuiState::default()
            };

            for mouse in [
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: files.x,
                    row: files.y,
                    modifiers: KeyModifiers::NONE,
                },
                MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: diff_area.x,
                    row: diff_area.y,
                    modifiers: KeyModifiers::NONE,
                },
                MouseEvent {
                    kind: MouseEventKind::Drag(MouseButton::Left),
                    column: diff_area.x,
                    row: diff_area.y.saturating_add(2),
                    modifiers: KeyModifiers::NONE,
                },
            ] {
                handle_mouse_event(mouse, size, &mut session, &mut mode, &mut tui_state);
            }

            assert_eq!(session.focus, Focus::Diff);
            assert_eq!(session.diff_cursor, before_cursor);
            assert_eq!(session.diff_scroll, 0);
            assert_eq!(session.diff_range_bounds(), None);
            assert_eq!(tui_state.diff_drag, None);
        }
    }

    #[test]
    fn wrapped_diff_mouse_continuations_and_wheel_use_visual_rows() {
        let long = "long unicode 界e\u{301} 👨‍👩‍👧‍👦 ".repeat(12);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        session.file_pane_visible = false;
        let owner = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == long)
            .unwrap();
        session.diff_scroll = owner as u16;
        let size = ratatui::prelude::Size::new(60, 8);
        let layout = ui_layout(
            Rect::new(0, 0, size.width, size.height),
            false,
            &UiConfig::default(),
            30,
        );
        let inner = inner_bordered(layout.diff);
        let mut tui_state = TuiState::default();

        // The second terminal row is a continuation of the same logical row.
        handle_left_down(
            inner.x + 12,
            inner.y + 1,
            layout,
            &mut session,
            &mut tui_state,
        );
        assert_eq!(session.diff_cursor, owner);
        assert_eq!(tui_state.diff_drag.unwrap().start_row, owner);

        let mut mode = Mode::Normal;
        handle_mouse_event(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: inner.x + 12,
                row: inner.y + 1,
                modifiers: KeyModifiers::NONE,
            },
            size,
            &mut session,
            &mut mode,
            &mut tui_state,
        );
        assert_eq!(session.diff_scroll as usize, owner);
        assert!(tui_state.diff_viewport.visual_state(&session).0 > 0);
    }

    const MENU_TEST_SIZE: ratatui::prelude::Size = ratatui::prelude::Size {
        width: 110,
        height: 24,
    };

    fn menu_test_state() -> TuiState {
        TuiState {
            layout_config: UiConfig {
                menu_bar: true,
                ..UiConfig::default()
            },
            terminal_size: MENU_TEST_SIZE,
            ..TuiState::default()
        }
    }

    fn menu_test_keymap() -> KeyMap {
        KeyMap::try_from(&KeybindingsConfig::default()).unwrap()
    }

    fn menu_index(title: &str) -> usize {
        menu::MENUS
            .iter()
            .position(|menu| menu.title == title)
            .unwrap()
    }

    fn menu_title_rect(keymap: &KeyMap, menu_area: Rect, title: &str) -> Rect {
        let index = menu_index(title);
        menu::bar_entries(keymap)
            .iter()
            .find(|entry| entry.menu_index == index)
            .and_then(|entry| menu::title_region(entry, menu_area))
            .unwrap()
    }

    fn menu_mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn dispatch_menu_mouse(
        mouse: MouseEvent,
        session: &mut ReviewSession,
        mode: &mut Mode,
        keymap: &KeyMap,
        tui_state: &mut TuiState,
    ) -> MenuMouseOutcome {
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        handle_menu_mouse_event(
            mouse,
            MENU_TEST_SIZE,
            session,
            mode,
            keymap,
            &loader,
            tui_state,
        )
        .unwrap()
    }

    #[test]
    fn menu_title_click_toggles_dropdown_and_click_away_swallows_and_closes() {
        let mut session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let keymap = menu_test_keymap();
        let mut tui_state = menu_test_state();
        let mut mode = Mode::Normal;
        let area = Rect::new(0, 0, MENU_TEST_SIZE.width, MENU_TEST_SIZE.height);
        let layout = tui_state.review_layout(&session, area);
        let title = menu_title_rect(&keymap, layout.menu, "hunk");

        let up = MouseEventKind::Up(MouseButton::Left);
        let outcome = dispatch_menu_mouse(
            menu_mouse(up, title.x, title.y),
            &mut session,
            &mut mode,
            &keymap,
            &mut tui_state,
        );
        assert_eq!(outcome, MenuMouseOutcome::Consumed);
        assert_eq!(tui_state.menu.open, Some(menu_index("hunk")));

        // Mouse-up on the same title closes it again.
        let outcome = dispatch_menu_mouse(
            menu_mouse(up, title.x, title.y),
            &mut session,
            &mut mode,
            &keymap,
            &mut tui_state,
        );
        assert_eq!(outcome, MenuMouseOutcome::Consumed);
        assert_eq!(tui_state.menu.open, None);

        // Reopen, then click away in the diff pane: the dropdown closes and
        // the closing click is swallowed instead of mutating the review.
        dispatch_menu_mouse(
            menu_mouse(up, title.x, title.y),
            &mut session,
            &mut mode,
            &keymap,
            &mut tui_state,
        );
        assert_eq!(tui_state.menu.open, Some(menu_index("hunk")));
        let diff_inner = inner_bordered(layout.diff);
        let outcome = dispatch_menu_mouse(
            menu_mouse(
                MouseEventKind::Down(MouseButton::Left),
                diff_inner.x + 2,
                diff_inner.y + 1,
            ),
            &mut session,
            &mut mode,
            &keymap,
            &mut tui_state,
        );
        assert_eq!(outcome, MenuMouseOutcome::Consumed);
        assert_eq!(tui_state.menu.open, None);
        assert_eq!(tui_state.diff_drag, None);

        // With the menu closed again the same event is not menu business.
        let outcome = dispatch_menu_mouse(
            menu_mouse(
                MouseEventKind::Down(MouseButton::Left),
                diff_inner.x + 2,
                diff_inner.y + 1,
            ),
            &mut session,
            &mut mode,
            &keymap,
            &mut tui_state,
        );
        assert_eq!(outcome, MenuMouseOutcome::Ignored);
    }

    #[test]
    fn menu_hover_switches_open_dropdown_and_highlights_items() {
        let mut session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let keymap = menu_test_keymap();
        let mut tui_state = menu_test_state();
        let mut mode = Mode::Normal;
        let area = Rect::new(0, 0, MENU_TEST_SIZE.width, MENU_TEST_SIZE.height);
        let layout = tui_state.review_layout(&session, area);
        tui_state.menu.open_menu(menu_index("hunk"));

        // Hovering another title switches the open dropdown to it.
        let file_title = menu_title_rect(&keymap, layout.menu, "file");
        let outcome = dispatch_menu_mouse(
            menu_mouse(MouseEventKind::Moved, file_title.x, file_title.y),
            &mut session,
            &mut mode,
            &keymap,
            &mut tui_state,
        );
        assert_eq!(outcome, MenuMouseOutcome::Consumed);
        assert_eq!(tui_state.menu.open, Some(menu_index("file")));
        assert_eq!(tui_state.menu.hovered, None);

        // Hovering a dropdown row highlights it; leaving clears it.
        let rect = menu::dropdown_rect(menu_index("file"), &keymap, layout.menu, area).unwrap();
        dispatch_menu_mouse(
            menu_mouse(MouseEventKind::Moved, rect.x + 1, rect.y + 2),
            &mut session,
            &mut mode,
            &keymap,
            &mut tui_state,
        );
        assert_eq!(tui_state.menu.hovered, Some(1));
        dispatch_menu_mouse(
            menu_mouse(MouseEventKind::Moved, rect.x + 1, rect.y + rect.height + 3),
            &mut session,
            &mut mode,
            &keymap,
            &mut tui_state,
        );
        assert_eq!(tui_state.menu.open, Some(menu_index("file")));
        assert_eq!(tui_state.menu.hovered, None);
    }

    #[test]
    fn menu_item_click_dispatches_the_same_action_as_the_bound_key() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let keymap = menu_test_keymap();
        let area = Rect::new(0, 0, MENU_TEST_SIZE.width, MENU_TEST_SIZE.height);

        // Mouse path: open "view" and click the "side-by-side" item.
        let mut session = snapshot_session(raw);
        let mut tui_state = menu_test_state();
        let mut mode = Mode::Normal;
        let layout = tui_state.review_layout(&session, area);
        let title = menu_title_rect(&keymap, layout.menu, "view");
        let up = MouseEventKind::Up(MouseButton::Left);
        dispatch_menu_mouse(
            menu_mouse(up, title.x, title.y),
            &mut session,
            &mut mode,
            &keymap,
            &mut tui_state,
        );
        let view = menu_index("view");
        let rect = menu::dropdown_rect(view, &keymap, layout.menu, area).unwrap();
        let items = menu::dropdown_items(view, &keymap);
        let item = items
            .iter()
            .position(|item| item.action == Action::ToggleDiffView)
            .unwrap();
        let outcome = dispatch_menu_mouse(
            menu_mouse(up, rect.x + 1, rect.y + 1 + item as u16),
            &mut session,
            &mut mode,
            &keymap,
            &mut tui_state,
        );
        assert_eq!(outcome, MenuMouseOutcome::Consumed);
        assert_eq!(tui_state.menu.open, None);
        assert!(matches!(mode, Mode::Normal));

        // Keyboard path: the bound key on a twin session.
        let mut twin = snapshot_session(raw);
        twin.focus = Focus::Diff;
        let mut twin_state = menu_test_state();
        let mut twin_mode = Mode::Normal;
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        handle_key_event(
            KeyEvent::from(KeyCode::Char('|')),
            &mut twin,
            &mut twin_mode,
            &keymap,
            &loader,
            &mut twin_state,
        )
        .unwrap();

        assert_eq!(
            twin.diff_cues.view,
            crate::config::DiffViewModeConfig::SideBySide
        );
        assert_eq!(session.diff_cues.view, twin.diff_cues.view);
    }

    #[test]
    fn menu_quit_item_requests_quit_through_normal_dispatch() {
        let mut session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let keymap = menu_test_keymap();
        let mut tui_state = menu_test_state();
        let mut mode = Mode::Normal;
        let area = Rect::new(0, 0, MENU_TEST_SIZE.width, MENU_TEST_SIZE.height);
        let layout = tui_state.review_layout(&session, area);
        tui_state.menu.open_menu(menu_index("quit"));
        let rect = menu::dropdown_rect(menu_index("quit"), &keymap, layout.menu, area).unwrap();
        let outcome = dispatch_menu_mouse(
            menu_mouse(
                MouseEventKind::Up(MouseButton::Left),
                rect.x + 1,
                rect.y + 1,
            ),
            &mut session,
            &mut mode,
            &keymap,
            &mut tui_state,
        );
        assert_eq!(outcome, MenuMouseOutcome::Quit);
        assert_eq!(tui_state.menu.open, None);
    }

    #[test]
    fn blocked_modal_state_ignores_menu_clicks() {
        let mut session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let keymap = menu_test_keymap();
        let mut tui_state = menu_test_state();
        let mut mode = Mode::Help;
        let area = Rect::new(0, 0, MENU_TEST_SIZE.width, MENU_TEST_SIZE.height);
        let layout = tui_state.review_layout(&session, area);
        let title = menu_title_rect(&keymap, layout.menu, "hunk");
        let outcome = dispatch_menu_mouse(
            menu_mouse(MouseEventKind::Up(MouseButton::Left), title.x, title.y),
            &mut session,
            &mut mode,
            &keymap,
            &mut tui_state,
        );
        assert_eq!(outcome, MenuMouseOutcome::Ignored);
        assert_eq!(tui_state.menu.open, None);
        assert!(matches!(mode, Mode::Help));
    }

    #[test]
    fn esc_closes_open_dropdown_before_clearing_other_transient_layers() {
        let mut session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let keymap = menu_test_keymap();
        let mut tui_state = menu_test_state();
        let mut mode = Mode::Normal;
        tui_state.menu.open_menu(menu_index("view"));
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Info,
            message: "transient".to_owned(),
        });
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };

        // First Esc: only the dropdown (topmost layer) closes.
        handle_key_event(
            KeyEvent::from(KeyCode::Esc),
            &mut session,
            &mut mode,
            &keymap,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert_eq!(tui_state.menu.open, None);
        assert!(tui_state.notice.is_some());
        assert!(matches!(mode, Mode::Normal));

        // Second Esc: the existing dismissal behavior is untouched.
        handle_key_event(
            KeyEvent::from(KeyCode::Esc),
            &mut session,
            &mut mode,
            &keymap,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert!(tui_state.notice.is_none());
    }

    #[test]
    fn opening_a_modal_closes_the_open_dropdown() {
        let mut session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let keymap = menu_test_keymap();
        let mut tui_state = menu_test_state();
        let mut mode = Mode::Normal;
        tui_state.menu.open_menu(menu_index("hunk"));
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        handle_key_event(
            KeyEvent::from(KeyCode::Char('?')),
            &mut session,
            &mut mode,
            &keymap,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert!(matches!(mode, Mode::Help));
        assert_eq!(tui_state.menu.open, None);
    }

    #[test]
    fn stream_mouse_landing_reanchors_after_detailed_rows_reshape_hit_projection() {
        let context = (0..8)
            .map(|line| format!(" fn before_{line}() {{}}\n"))
            .collect::<String>();
        let raw = format!(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -10,12 +10,12 @@\n{context}-fn old_target() {{}}\n+fn new_target() {{}}\n fn after_0() {{}}\n fn after_1() {{}}\n fn after_2() {{}}\ndiff --git a/c.rs b/c.rs\n--- a/c.rs\n+++ b/c.rs\n@@ -1,8 +1,8 @@\n-old_0\n+new_0\n-old_1\n+new_1\n-old_2\n+new_2\n-old_3\n+new_3\n"
        );
        let mut session = snapshot_session(&raw);
        session.stream_mode = true;
        session.fold_context = true;
        session.file_pane_visible = false;
        let hit = session
            .review_stream()
            .rows
            .iter()
            .position(|row| {
                row.path.as_deref() == Some("b.rs")
                    && row
                        .anchor
                        .as_ref()
                        .and_then(crate::anchor::CommentAnchor::line)
                        == Some(18)
            })
            .unwrap();
        session.stream_scroll = hit as u16;
        let layout = ui_layout(Rect::new(0, 0, 100, 12), false, &UiConfig::default(), 30);
        let inner = inner_bordered(layout.diff);
        let mut tui_state = TuiState::default();
        tui_state.diff_viewport.file_restored(&session);

        handle_left_down(inner.x + 15, inner.y, layout, &mut session, &mut tui_state);

        assert_eq!(session.selected_file().unwrap().path, "b.rs");
        let stream_row = session.selected_stream_row().unwrap();
        assert_eq!(
            stream_row
                .anchor
                .as_ref()
                .and_then(crate::anchor::CommentAnchor::line),
            Some(18)
        );
        assert_eq!(
            session.diff_rows_for_selected_file()[session.diff_cursor].anchor,
            stream_row.anchor
        );
        assert!(
            session
                .diff_rows_for_selected_file()
                .iter()
                .any(|row| matches!(row.kind, crate::app::DiffRowKind::ContextFold))
        );
    }

    #[test]
    fn split_mouse_hit_testing_selects_the_clicked_logical_side() {
        let removed = "removed side ".repeat(10);
        let added = "added side ".repeat(10);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-{removed}\n+{added}\n"
        ));
        session.toggle_diff_view();
        session.file_pane_visible = false;
        let rows = session.diff_rows_for_selected_file();
        let left = rows.iter().position(|row| row.text == removed).unwrap();
        let right = rows.iter().position(|row| row.text == added).unwrap();
        session.diff_scroll = left as u16;
        let size = ratatui::prelude::Size::new(130, 6);
        let layout = ui_layout(
            Rect::new(0, 0, size.width, size.height),
            false,
            &UiConfig::default(),
            30,
        );
        let inner = inner_bordered(layout.diff);
        let mut tui_state = TuiState::default();

        handle_left_down(
            inner.x + 10,
            inner.y + 1,
            layout,
            &mut session,
            &mut tui_state,
        );
        assert_eq!(session.diff_cursor, left);
        handle_left_down(
            inner.x + inner.width - 10,
            inner.y + 1,
            layout,
            &mut session,
            &mut tui_state,
        );
        assert_eq!(session.diff_cursor, right);
    }

    #[test]
    fn dragging_within_wrapped_row_keeps_logical_range_single_line() {
        let long = "same logical row ".repeat(20);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        session.file_pane_visible = false;
        let owner = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == long)
            .unwrap();
        session.diff_scroll = owner as u16;
        let layout = ui_layout(Rect::new(0, 0, 60, 7), false, &UiConfig::default(), 30);
        let inner = inner_bordered(layout.diff);
        let mut tui_state = TuiState::default();

        handle_left_down(inner.x + 10, inner.y, layout, &mut session, &mut tui_state);
        handle_left_drag(
            inner.x + 10,
            inner.y + 2,
            layout,
            &mut session,
            &mut tui_state,
        );

        assert_eq!(session.diff_cursor, owner);
        assert_eq!(session.diff_range_bounds(), Some((owner, owner)));
        assert_eq!(tui_state.diff_drag.unwrap().current_row, owner);
    }

    #[test]
    fn mouse_drag_start_uses_normalized_commentable_cursor() {
        let long = "wrapped resize ".repeat(30);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        let row = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == long)
            .unwrap();
        session.diff_scroll = row as u16;
        session.diff_cursor = row;
        session.file_pane_visible = false;
        let rows = session.diff_rows_for_selected_file();
        let hunk = rows
            .iter()
            .position(|row| matches!(row.kind, crate::app::DiffRowKind::HunkHeader))
            .unwrap();
        let layout = ui_layout(Rect::new(0, 0, 80, 12), false, &UiConfig::default(), 30);
        let inner = inner_bordered(layout.diff);
        let mut tui_state = TuiState::default();

        handle_left_down(
            inner.x + 2,
            inner.y + hunk as u16,
            layout,
            &mut session,
            &mut tui_state,
        );

        let drag = tui_state.diff_drag.unwrap();
        assert_eq!(drag.start_row, session.diff_cursor);
        assert!(rows[drag.start_row].anchor.is_some());
        assert_ne!(drag.start_row, hunk);
    }

    #[test]
    fn blank_split_continuation_does_not_start_mouse_drag() {
        let removed = "long removed side ".repeat(20);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-{removed}\n+short\n"
        ));
        session.toggle_diff_view();
        session.file_pane_visible = false;
        let rows = session.diff_rows_for_selected_file();
        let left = rows.iter().position(|row| row.text == removed).unwrap();
        session.diff_scroll = left as u16;
        let layout = ui_layout(Rect::new(0, 0, 130, 8), false, &UiConfig::default(), 30);
        let inner = inner_bordered(layout.diff);
        let mut tui_state = TuiState::default();

        handle_left_down(
            inner.x + inner.width - 4,
            inner.y + 1,
            layout,
            &mut session,
            &mut tui_state,
        );

        assert!(tui_state.diff_drag.is_none());
    }

    #[test]
    fn fingerprint_events_report_range_updates_and_at_moves() {
        let events = fingerprint_events(
            "@ old c0\nold c0\nstay c1\nrewrite c2\n",
            "@ new c9\nstay c1\nrewrite c3\nnew c4\n",
        );
        assert!(events.iter().any(|event| event == "@ moved to new"));
        assert!(
            events
                .iter()
                .any(|event| event == "change new entered range")
        );
        assert!(events.iter().any(|event| event == "change rewrite updated"));
        assert!(events.iter().any(|event| event == "change old left range"));
    }

    #[test]
    fn description_only_refresh_events_name_description_updates() {
        let mut events = vec!["change rewrite updated".to_owned()];

        specialize_description_only_events(&mut events, true);

        assert_eq!(events, vec!["change rewrite description updated"]);
    }

    #[test]
    fn refresh_file_events_name_reverts_to_seen_content() {
        let event = refresh_file_event(&crate::app::RefreshedFileChange {
            path: "src/worker.rs".to_owned(),
            is_new: false,
            additions_delta: -2,
            deletions_delta: 0,
            was_reviewed: false,
            reverted_to_seen: true,
        });

        assert_eq!(event, "src/worker.rs reverted to previously seen content");
    }

    #[test]
    fn live_refresh_runs_under_read_only_popups() {
        assert!(mode_allows_live_refresh(&Mode::Normal));
        assert!(mode_allows_live_refresh(&Mode::Activity(
            ActivityListState::new()
        )));
        assert!(mode_allows_live_refresh(&Mode::Help));
    }

    const CONTEXT_EXPANSION_DIFF: &str = r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -4,3 +4,3 @@
 line 4
-old five
+line 5
 line 6
@@ -12,3 +12,3 @@
 line 12
-old thirteen
+line 13
 line 14
"#;

    #[test]
    fn expand_context_fetches_file_contents_and_expands_nearest_gap() {
        let mut session = snapshot_session(CONTEXT_EXPANSION_DIFF);
        session.toggle_focus();
        let mut backend = MockJjBackend::with_diff(Ok(String::new()));
        backend.file_contents = Some((1..=20).map(|n| format!("line {n}\n")).collect());
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let mut mode = Mode::Normal;

        handle_normal_action(
            Action::ExpandContext,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();

        assert!(tui_state.notice.is_none());
        assert!(session.file_contents_loaded("a.txt"));
        // Ten lines fit the nearest (top) gap of three: it fully opens and
        // lines 1-3 appear with real numbering.
        let rows = session.diff_rows_for_selected_file();
        assert!(rows.iter().any(|row| row.text == "line 1"));
        assert!(
            rows.iter()
                .any(|row| matches!(row.kind, crate::app::DiffRowKind::ExpandGap { .. }))
        );
    }

    #[test]
    fn context_expansion_reconciles_measured_cursor_visibility() {
        let mut session = snapshot_session(CONTEXT_EXPANSION_DIFF);
        session.toggle_focus();
        let cursor = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "line 12")
            .unwrap();
        session.select_diff_row(cursor);
        let mut backend = MockJjBackend::with_diff(Ok(String::new()));
        backend.file_contents = Some((1..=20).map(|n| format!("line {n}\n")).collect());
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(60, 8),
            ..TuiState::default()
        };
        let inner = current_diff_inner(&session, &tui_state);
        tui_state
            .diff_viewport
            .logical_selection(&mut session, inner);
        let mut mode = Mode::Normal;

        handle_normal_action(
            Action::ExpandContext,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();

        assert_eq!(
            session.diff_rows_for_selected_file()[session.diff_cursor].text,
            "line 12"
        );
        assert!(diff_cursor_is_visible(
            &session,
            current_diff_inner(&session, &tui_state),
            &tui_state,
        ));
    }

    #[test]
    fn wrap_handler_preserves_a_detached_manual_viewport() {
        let mut body = String::from(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,20 +1,20 @@\n",
        );
        for line in 1..=20 {
            body.push_str(&format!(" line {line} {}\n", "wide ".repeat(10)));
        }
        let mut session = snapshot_session(&body);
        session.focus = Focus::Diff;
        session.diff_cursor = session
            .diff_rows_for_selected_file()
            .iter()
            .rposition(|row| row.anchor.is_some())
            .unwrap();
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(50, 8),
            ..TuiState::default()
        };
        let inner = current_diff_inner(&session, &tui_state);
        tui_state.diff_viewport.reflow(&mut session, inner, false);
        let detached_top = session.diff_top_identity();
        assert!(!diff_cursor_is_visible(&session, inner, &tui_state));
        let mut mode = Mode::Normal;

        handle_normal_action(
            Action::ToggleDiffWrap,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();

        assert_eq!(session.diff_top_identity(), detached_top);
        assert!(!diff_cursor_is_visible(
            &session,
            current_diff_inner(&session, &tui_state),
            &tui_state,
        ));
    }

    fn split_reflow_session() -> ReviewSession {
        let mut body = String::from(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,30 +1,30 @@\n",
        );
        for line in 1..=30 {
            body.push_str(&format!(" line {line} {}\n", "wide ".repeat(12)));
        }
        snapshot_session(&body)
    }

    #[test]
    fn widen_file_pane_reflows_and_keeps_followed_cursor_visible() {
        let mut session = split_reflow_session();
        session.focus = Focus::Diff;
        session.diff_cursor = session
            .diff_rows_for_selected_file()
            .iter()
            .rposition(|row| row.anchor.is_some())
            .unwrap();
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(100, 10),
            ..TuiState::default()
        };
        let old_inner = current_diff_inner(&session, &tui_state);
        tui_state
            .diff_viewport
            .logical_selection(&mut session, old_inner);
        assert!(diff_cursor_is_visible(&session, old_inner, &tui_state));
        let mut mode = Mode::Normal;

        handle_normal_action(
            Action::WidenFilePane,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();

        assert_eq!(tui_state.file_pane.split_percent, 35);
        let widened_inner = current_diff_inner(&session, &tui_state);
        assert!(widened_inner.width < old_inner.width);
        assert!(diff_cursor_is_visible(&session, widened_inner, &tui_state));
    }

    #[test]
    fn narrow_file_pane_reflows_without_reattaching_detached_scroll() {
        let mut session = split_reflow_session();
        session.focus = Focus::Diff;
        session.diff_cursor = session
            .diff_rows_for_selected_file()
            .iter()
            .rposition(|row| row.anchor.is_some())
            .unwrap();
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(100, 10),
            ..TuiState::default()
        };
        let old_inner = current_diff_inner(&session, &tui_state);
        tui_state
            .diff_viewport
            .logical_selection(&mut session, old_inner);
        tui_state
            .diff_viewport
            .visual_scroll(&mut session, old_inner, isize::MIN);
        let detached_top = session.diff_top_identity();
        assert!(!diff_cursor_is_visible(&session, old_inner, &tui_state));
        let mut mode = Mode::Normal;

        handle_normal_action(
            Action::NarrowFilePane,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();

        assert_eq!(tui_state.file_pane.split_percent, 25);
        let narrowed_inner = current_diff_inner(&session, &tui_state);
        assert!(narrowed_inner.width > old_inner.width);
        assert_eq!(session.diff_top_identity(), detached_top);
        assert!(!diff_cursor_is_visible(
            &session,
            narrowed_inner,
            &tui_state
        ));
    }

    #[test]
    fn effective_file_pane_resolves_preference_breakpoint_and_override() {
        let mut session = snapshot_session("");
        let mut tui_state = TuiState::default();
        assert!(session.file_pane_visible);
        assert!(!tui_state.effective_file_pane(&session, 49).visible);
        assert!(tui_state.effective_file_pane(&session, 50).visible);

        session.file_pane_visible = false;
        assert!(!tui_state.effective_file_pane(&session, 100).visible);
        tui_state.file_pane.explicit_override = Some(true);
        assert!(tui_state.effective_file_pane(&session, 20).visible);
        tui_state.file_pane.explicit_override = Some(false);
        session.file_pane_visible = true;
        assert!(!tui_state.effective_file_pane(&session, 100).visible);
    }

    #[test]
    fn tui_state_default_uses_production_thirty_percent_file_split() {
        let session = snapshot_session("");
        let tui_state = TuiState::default();
        assert_eq!(tui_state.file_pane.split_percent, 30);
        let layout = tui_state.review_layout(&session, Rect::new(0, 0, 100, 20));
        assert_eq!(layout.files.width, 30);
        assert_eq!(layout.diff.width, 70);
    }

    #[test]
    fn equal_startup_size_corrects_auto_hidden_files_focus() {
        let mut session = snapshot_session("");
        assert_eq!(session.focus, Focus::Files);
        let size = ratatui::prelude::Size::new(40, 12);
        let mut mode = Mode::Normal;
        let mut tui_state = TuiState {
            terminal_size: size,
            ..TuiState::default()
        };

        observe_terminal_size(size, &mut session, &mut mode, &mut tui_state);

        assert_eq!(session.focus, Focus::Diff);
        assert!(!tui_state.effective_file_pane(&session, size.width).visible);
    }

    #[test]
    fn forced_visible_narrow_pane_persists_across_resize() {
        let mut session = snapshot_session("");
        let mut mode = Mode::Normal;
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(40, 12),
            ..TuiState::default()
        };
        tui_state.toggle_file_pane(&mut session, 40);
        session.focus = Focus::Files;

        observe_terminal_size(
            ratatui::prelude::Size::new(35, 12),
            &mut session,
            &mut mode,
            &mut tui_state,
        );

        assert_eq!(tui_state.file_pane.explicit_override, Some(true));
        assert!(tui_state.effective_file_pane(&session, 35).visible);
        assert_eq!(session.focus, Focus::Files);
    }

    #[test]
    fn large_diff_toggle_finishes_with_cursor_visible_in_short_pane() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n-old 1\n-old 2\n-old 3\n+new 1\n+new 2\n+new 3\n",
        );
        session.focus = Focus::Diff;
        session.max_diff_lines = 1;
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(30, 4),
            ..TuiState::default()
        };
        let mut mode = Mode::Normal;

        handle_normal_action(
            Action::ToggleLargeDiff,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();

        assert!(diff_cursor_is_visible(
            &session,
            current_diff_inner(&session, &tui_state),
            &tui_state,
        ));
    }

    #[test]
    fn observed_resize_transition_ignores_stale_event_dimensions() {
        let queued = ratatui::prelude::Size::new(50, 8);
        let observed = ratatui::prelude::Size::new(90, 12);
        assert_eq!(resize_event_observed_size(queued, observed), observed);

        let long = "wrapped resize row ".repeat(30);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        let row = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == long)
            .unwrap();
        session.diff_scroll = row as u16;
        session.diff_cursor = row;
        let mut mode = Mode::Normal;
        let mut tui_state = TuiState {
            terminal_size: queued,
            ..TuiState::default()
        };
        let old_inner = current_diff_inner(&session, &tui_state);
        tui_state
            .diff_viewport
            .visual_scroll(&mut session, old_inner, 2);
        let top = session.diff_top_identity();
        let continuation = tui_state.diff_viewport.visual_state(&session).0;
        assert!(continuation > 0);
        dispatch_resize_event(queued, observed, &mut session, &mut mode, &mut tui_state);
        assert_eq!(tui_state.terminal_size, observed);
        assert_eq!(session.diff_top_identity(), top);
        assert!(tui_state.diff_viewport.visual_state(&session).0 <= continuation);

        let followed_inner = current_diff_inner(&session, &tui_state);
        tui_state
            .diff_viewport
            .logical_selection(&mut session, followed_inner);
        let followed_top = session.diff_top_identity();
        let newer = ratatui::prelude::Size::new(40, 7);
        dispatch_resize_event(queued, newer, &mut session, &mut mode, &mut tui_state);
        assert_eq!(tui_state.terminal_size, newer);
        assert_eq!(session.diff_top_identity(), followed_top);
        assert!(diff_cursor_is_visible(
            &session,
            current_diff_inner(&session, &tui_state),
            &tui_state,
        ));
    }

    #[test]
    fn style_only_view_option_does_not_measure_or_reflow_viewport() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(80, 12),
            ..TuiState::default()
        };
        let inner = current_diff_inner(&session, &tui_state);
        let _ = tui_state.diff_viewport.cursor_is_visible(&session, inner);
        let builds = tui_state.diff_viewport.cache_builds();
        let mut options = ViewOptionsState::default();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        assert!(!handle_view_options_key(
            KeyEvent::from(KeyCode::Enter),
            &mut options,
            &mut session,
            &keymap,
            &mut tui_state,
        ));
        assert_eq!(tui_state.diff_viewport.cache_builds(), builds);
    }

    #[test]
    fn context_actions_are_inert_while_files_have_focus() {
        let mut session = snapshot_session(CONTEXT_EXPANSION_DIFF);
        let mut backend = MockJjBackend::with_diff(Ok(String::new()));
        backend.file_contents = Some((1..=20).map(|n| format!("line {n}\n")).collect());
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let mut mode = Mode::Normal;

        for action in [
            Action::ExpandContext,
            Action::ExpandContextAll,
            Action::CollapseContext,
        ] {
            handle_normal_action(action, &mut session, &mut mode, &loader, &mut tui_state).unwrap();
        }

        assert!(!session.has_file_contents_entry("a.txt"));
        assert!(tui_state.notice.is_none());
    }

    #[test]
    fn help_scroll_keys_do_not_close_until_escape() {
        let mut session = snapshot_session("");
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let mut tui_state = TuiState {
            diff_drag: Some(DiffDrag {
                start_row: 0,
                start_file_index: 0,
                current_row: 0,
                saw_drag: false,
            }),
            ..TuiState::default()
        };
        let mut mode = Mode::Help;

        handle_key_event(
            KeyEvent::from(KeyCode::Char('j')),
            &mut session,
            &mut mode,
            &keymap,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert!(matches!(mode, Mode::Help));
        assert_eq!(tui_state.help_scroll, 1);
        assert_eq!(tui_state.diff_drag, None);

        handle_key_event(
            KeyEvent::from(KeyCode::Esc),
            &mut session,
            &mut mode,
            &keymap,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert!(matches!(mode, Mode::Normal));
    }

    #[test]
    fn expand_context_records_fetch_failures_as_notices() {
        let mut session = snapshot_session(CONTEXT_EXPANSION_DIFF);
        session.toggle_focus();
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let mut mode = Mode::Normal;

        handle_normal_action(
            Action::ExpandContext,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();

        assert!(
            tui_state
                .notice
                .unwrap()
                .message
                .contains("context expansion unavailable")
        );
        // The failure is remembered: gap rows stop offering expansion.
        assert!(session.has_file_contents_entry("a.txt"));
        assert!(!session.file_contents_loaded("a.txt"));
        let rows = session.diff_rows_for_selected_file();
        let gap_row = rows
            .iter()
            .find(|row| matches!(row.kind, crate::app::DiffRowKind::ExpandGap { .. }))
            .unwrap();
        assert!(!gap_row.text.contains("expand"));
    }

    #[test]
    fn collapse_context_without_expansion_shows_a_notice() {
        let mut session = snapshot_session(CONTEXT_EXPANSION_DIFF);
        session.toggle_focus();
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let mut mode = Mode::Normal;

        handle_normal_action(
            Action::CollapseContext,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();

        assert!(
            tui_state
                .notice
                .unwrap()
                .message
                .contains("no expanded context to collapse")
        );
    }

    #[test]
    fn large_change_nudge_appends_to_load_notice() {
        let mut session = snapshot_session("");
        session.nudge_files = 1;
        let backend = MockJjBackend::with_diff(Ok(r#"diff --git a/new.rs b/new.rs
--- a/new.rs
+++ b/new.rs
@@ -1 +1 @@
-old
+new
"#
        .to_owned()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();

        load_review_target(
            &loader,
            &mut session,
            ReviewTarget::parent_to_current(),
            &mut tui_state,
        );

        let notice = tui_state.notice.unwrap();
        assert!(notice.message.contains("loaded @-..@"));
        assert!(notice.message.contains("large change"));
    }

    #[test]
    fn target_chooser_loads_selected_base_to_current_tip() {
        let mut session = snapshot_session("");
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let mut chooser = TargetChooserState::new(
            vec![JjChangeSummary {
                change_id: "mainchange".to_owned(),
                bookmarks: "main".to_owned(),
                description: "mainline".to_owned(),
            }],
            "trunk()",
            "@",
        );
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert!(handle_target_chooser_key(
            KeyEvent::from(KeyCode::Enter),
            &mut chooser,
            &mut session,
            &keymap,
            &loader,
            &mut tui_state,
        ));

        assert_eq!(
            backend.calls.borrow().as_slice(),
            [ReviewTarget::new("mainchange", "@")]
        );
        assert_eq!(session.target, ReviewTarget::new("mainchange", "@"));
    }

    #[test]
    fn target_chooser_filter_accepts_g_and_shifted_characters() {
        let mut session = snapshot_session("");
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let mut chooser = TargetChooserState::new(
            vec![
                JjChangeSummary {
                    change_id: "abc".to_owned(),
                    bookmarks: "main".to_owned(),
                    description: "feature work".to_owned(),
                },
                JjChangeSummary {
                    change_id: "def".to_owned(),
                    bookmarks: String::new(),
                    description: "generated Goo".to_owned(),
                },
            ],
            "abc",
            "@",
        );
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        for key in [
            KeyEvent::from(KeyCode::Char('g')),
            KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT),
        ] {
            assert!(!handle_target_chooser_key(
                key,
                &mut chooser,
                &mut session,
                &keymap,
                &loader,
                &mut tui_state,
            ));
        }

        assert_eq!(chooser.query, "gG");
        assert_eq!(chooser.filtered, vec![1]);
    }

    const POLLING_DIFF: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old\n+new\n";
    const REFRESHED_DIFF: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+newer\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/c.rs b/c.rs\n--- a/c.rs\n+++ b/c.rs\n@@ -1 +1 @@\n-old\n+new\n";

    fn polling_session() -> ReviewSession {
        snapshot_session(POLLING_DIFF)
    }

    fn polling_loader(backend: &MockJjBackend) -> ReviewLoader<'_> {
        ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: backend,
        }
    }

    #[test]
    fn annotation_selection_tracks_exact_owner_across_click_and_keyboard_navigation() {
        let raw = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n-old one\n-old two\n+new one\n+new two\n";
        let mut session = attention_session(raw, "a.txt", Salience::Supporting);
        session.file_pane_visible = false;
        session.focus = Focus::Diff;
        session.agent_identity = crate::state::Identity {
            kind: AuthorKind::Agent,
            name: "configured-review-agent".into(),
        };
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let configured_agent = session.agent_identity.clone();
        let first = crate::attention::target_for_diff(&files, "a.txt", Some(1), None).unwrap();
        let second = crate::attention::target_for_diff(&files, "a.txt", Some(2), None).unwrap();
        session.durable_sessions_mut()[0].attention_regions.clear();
        session.durable_sessions_mut()[0].walkthroughs = vec![crate::state::Walkthrough {
            id: "guided".into(),
            steps: vec![WalkthroughStep {
                id: "guided-lines".into(),
                author: Some(configured_agent.clone()),
                title: Some("Agent-guided lines".into()),
                why: Some("Each line has its own card owner".into()),
                body: Some("Use the card at the current line".into()),
                artifacts: vec![crate::state::StepArtifact {
                    title: "evidence".into(),
                    kind: crate::state::StepArtifactKind::Example,
                    body: "CURRENT ROW ARTIFACT".into(),
                }],
                target: first,
                extra_targets: vec![second],
                ..Default::default()
            }],
            ..Default::default()
        }];
        crate::attention::sync_agent_attention(&mut session.durable_sessions_mut()[0], &files)
            .unwrap();
        let durable = review::active_session_for_loaded_review(
            session.durable_sessions(),
            &session.repo,
            &session.target.base,
            &session.target.rev,
        )
        .unwrap();
        let persisted_guided = durable.walkthroughs[0]
            .steps
            .iter()
            .find(|step| step.id == "guided-lines")
            .unwrap();
        assert_eq!(persisted_guided.author.as_ref(), Some(&configured_agent));
        assert!(persisted_guided.target.anchor.is_some());
        assert_eq!(persisted_guided.extra_targets.len(), 1);
        assert!(persisted_guided.extra_targets[0].anchor.is_some());
        let effective = crate::attention::resolve_effective_attention(
            durable,
            &persisted_guided.target,
            &files,
        );
        assert_eq!(effective.salience, crate::state::Salience::Spotlight);
        assert_eq!(effective.source, Some(crate::state::SalienceSource::Agent));
        let projected_card = annotation_card::AnnotationCard::from_walkthrough_step(
            persisted_guided,
            &persisted_guided.target,
            0,
            None,
            false,
        );
        let projected_layout = projected_card.layout(
            72,
            annotation_card::AnnotationCardDensity::Expanded,
            false,
            "E",
        );
        let rendered_card = (0..projected_layout.len())
            .map(|index| projected_layout.plain_line(index))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered_card.contains("agent:configured-review-agent"));

        let size = ratatui::prelude::Size::new(100, 36);
        let mut tui_state = TuiState {
            terminal_size: size,
            ..TuiState::default()
        };
        let layout = tui_state.review_layout(&session, Rect::new(0, 0, size.width, size.height));
        let inner = inner_bordered(layout.diff);
        let first_source = annotation_card::AnnotationSource::Walkthrough {
            step_id: "guided-lines".into(),
            part: 0,
        };
        let visible = tui_state
            .diff_viewport
            .annotation_visible_row(&session, inner, &first_source)
            .expect("first agent card visible");
        let mut mode = Mode::Normal;
        handle_mouse_event(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: inner.x + 4,
                row: inner.y + visible as u16,
                modifiers: KeyModifiers::NONE,
            },
            size,
            &mut session,
            &mut mode,
            &mut tui_state,
        );
        assert_eq!(
            tui_state.diff_viewport.selected_annotation_source(&session),
            Some(first_source)
        );
        assert!(selected_onboarding_target(&session, &tui_state));
        assert_eq!(
            inferred_comment_channel(
                &session,
                &tui_state,
                selected_onboarding_target(&session, &tui_state),
                None,
            ),
            Channel::Delegation
        );
        render::reconcile_diff_viewport(
            &mut session,
            Rect::new(
                inner.x,
                inner.y,
                inner.width.saturating_sub(12),
                inner.height,
            ),
            true,
            &tui_state,
        );
        assert_eq!(
            tui_state.diff_viewport.selected_annotation_source(&session),
            Some(annotation_card::AnnotationSource::Walkthrough {
                step_id: "guided-lines".into(),
                part: 0,
            }),
            "layout reflow on the same owner must preserve exact card selection"
        );

        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        handle_normal_action(
            Action::MoveDown,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        let second_owner = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.new_lineno == Some(2))
            .unwrap();
        assert_eq!(session.diff_cursor, second_owner);
        assert_eq!(
            tui_state.diff_viewport.selected_annotation_source(&session),
            None
        );
        assert!(!selected_onboarding_target(&session, &tui_state));
        assert_eq!(
            inferred_comment_channel(&session, &tui_state, false, None),
            Channel::Note
        );

        handle_normal_action(
            Action::ToggleAnnotationArtifacts,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        handle_normal_action(
            Action::MoveUp,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert_eq!(
            tui_state
                .diff_viewport
                .toggle_annotation_artifacts(&session),
            Some(true),
            "E on the second owner must not expand the formerly selected first card"
        );
        handle_normal_action(
            Action::MoveDown,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();
        assert_eq!(
            tui_state
                .diff_viewport
                .toggle_annotation_artifacts(&session),
            Some(false),
            "the current-row card was the one expanded by E"
        );
    }

    #[test]
    fn attention_focus_pins_current_spotlight_and_preserves_card_expansion() {
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let mut session = attention_session(raw, "a.rs", Salience::Spotlight);
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let target = crate::attention::target_for_diff(&files, "a.rs", Some(1), None).unwrap();
        session.durable_sessions_mut()[0]
            .walkthroughs
            .push(crate::state::Walkthrough {
                id: "walk".into(),
                steps: vec![WalkthroughStep {
                    id: "spot".into(),
                    target: target.clone(),
                    title: Some("Current narration".into()),
                    why: Some("This is the mental-model delta".into()),
                    artifacts: vec![crate::state::StepArtifact {
                        title: "example".into(),
                        kind: crate::state::StepArtifactKind::Example,
                        body: "expanded body".into(),
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            });
        let owner = session.stream_walkthrough_card_owner(&target).unwrap();
        session.select_stream_row(owner, false);
        let mut tui_state = TuiState {
            terminal_size: ratatui::prelude::Size::new(100, 24),
            ..TuiState::default()
        };
        assert_eq!(
            tui_state
                .diff_viewport
                .toggle_annotation_artifacts(&session),
            Some(true)
        );

        tui_state.enter_attention_focus(&mut session);
        assert_eq!(
            tui_state.diff_viewport.selected_annotation_source(&session),
            Some(annotation_card::AnnotationSource::Walkthrough {
                step_id: "spot".into(),
                part: 0,
            })
        );
        tui_state.leave_attention_focus(&mut session);
        assert_eq!(
            tui_state
                .diff_viewport
                .toggle_annotation_artifacts(&session),
            Some(false),
            "the exact pre-Focus artifact expansion was restored"
        );
    }

    #[test]
    fn persistent_fingerprint_failures_surface_an_error_then_recovery() {
        let mut session = polling_session();
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = polling_loader(&backend);
        let mut tui_state = TuiState::default();

        *backend.fingerprint.borrow_mut() =
            Err("jj log failed fingerprinting trunk()..@".to_owned());
        for tick in 1..FINGERPRINT_FAILURE_NOTICE_THRESHOLD {
            tui_state.last_repo_poll = None;
            maybe_refresh_review(&loader, &mut session, &mut tui_state);
            assert!(tui_state.notice.is_none(), "quiet failure #{tick}");
        }
        tui_state.last_repo_poll = None;
        maybe_refresh_review(&loader, &mut session, &mut tui_state);
        let notice = tui_state.notice.clone().expect("failure notice");
        assert_eq!(notice.level, UiNoticeLevel::Error);
        assert!(notice.message.contains("live refresh is failing"));
        assert!(notice.message.contains("jj log failed fingerprinting"));

        // Further failures do not re-post (no footer spam).
        tui_state.notice = None;
        tui_state.last_repo_poll = None;
        maybe_refresh_review(&loader, &mut session, &mut tui_state);
        assert!(tui_state.notice.is_none());

        // Recovery replaces the warning and resets the counter.
        *backend.fingerprint.borrow_mut() = Ok("baseline".to_owned());
        tui_state.last_repo_poll = None;
        maybe_refresh_review(&loader, &mut session, &mut tui_state);
        let notice = tui_state.notice.clone().expect("recovery notice");
        assert_eq!(notice.level, UiNoticeLevel::Info);
        assert!(notice.message.contains("live refresh recovered"));
        assert_eq!(tui_state.fingerprint_failures, 0);
    }

    #[test]
    fn repo_polling_baselines_then_refreshes_in_place_on_change() {
        let mut session = polling_session();
        session.file_pane_visible = false;
        let backend = MockJjBackend::with_diff(Ok(REFRESHED_DIFF.to_owned()));
        let loader = polling_loader(&backend);
        let mut tui_state = TuiState::default();

        // First poll only records the baseline: no reload, no notice.
        maybe_refresh_review(&loader, &mut session, &mut tui_state);
        assert!(backend.calls.borrow().is_empty());
        assert!(tui_state.notice.is_none());

        // Unchanged fingerprint: still nothing.
        tui_state.last_repo_poll = None;
        maybe_refresh_review(&loader, &mut session, &mut tui_state);
        assert!(backend.calls.borrow().is_empty());

        // New work landed: the review reloads in place, preserving view
        // state (hidden file pane) and picking up the new file.
        *backend.fingerprint.borrow_mut() = Ok("changed".to_owned());
        tui_state.last_repo_poll = None;
        maybe_refresh_review(&loader, &mut session, &mut tui_state);
        assert_eq!(backend.calls.borrow().len(), 1);
        assert!(!session.file_pane_visible);
        assert!(session.files.iter().any(|file| file.path == "c.rs"));
        assert!(
            tui_state
                .notice
                .unwrap()
                .message
                .contains("repository changed")
        );
    }

    #[test]
    fn repo_polling_is_throttled_and_ignores_fingerprint_errors() {
        let mut session = polling_session();
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = polling_loader(&backend);
        let mut tui_state = TuiState::default();

        maybe_refresh_review(&loader, &mut session, &mut tui_state);
        *backend.fingerprint.borrow_mut() = Ok("changed".to_owned());

        // Within the poll interval: the change is not even inspected.
        maybe_refresh_review(&loader, &mut session, &mut tui_state);
        assert!(backend.calls.borrow().is_empty());

        // Transient jj failures skip the tick without noise or baseline
        // loss.
        *backend.fingerprint.borrow_mut() = Err("locked".to_owned());
        tui_state.last_repo_poll = None;
        maybe_refresh_review(&loader, &mut session, &mut tui_state);
        assert!(backend.calls.borrow().is_empty());
        assert!(tui_state.notice.is_none());
    }

    #[test]
    fn repo_polling_notice_and_activity_include_specific_events() {
        let mut session = polling_session();
        session.files[0].viewed = true;
        let backend = MockJjBackend::with_diff(Ok(REFRESHED_DIFF.to_owned()));
        let loader = polling_loader(&backend);
        let mut tui_state = TuiState::default();
        *backend.fingerprint.borrow_mut() = Ok("@ old c0\nold c0\n".to_owned());
        maybe_refresh_review(&loader, &mut session, &mut tui_state);
        *backend.fingerprint.borrow_mut() = Ok("@ new c1\nnew c1\n".to_owned());
        tui_state.last_repo_poll = None;
        maybe_refresh_review(&loader, &mut session, &mut tui_state);
        let notice = tui_state.notice.unwrap().message;
        assert!(notice.contains("@ moved to new"));
        assert!(notice.contains("change new entered range"));
        assert!(notice.contains("change old left range"));
        assert!(notice.contains("1 viewed file changed — needs re-review"));
        assert!(
            tui_state
                .activity
                .iter()
                .any(|event| event.message == "@ moved to new")
        );
        // Only the file that actually changed in this refresh is named:
        // c.rs is new; a.rs/b.rs carried identical content and stay quiet.
        assert!(
            tui_state
                .activity
                .iter()
                .any(|event| event.message == "c.rs appeared (+1 −1)")
        );
        assert!(
            tui_state
                .activity
                .iter()
                .any(|event| event.message == "a.rs updated — was viewed, needs re-review")
        );
        assert!(
            !tui_state
                .activity
                .iter()
                .any(|event| event.message.starts_with("b.rs"))
        );
    }
}
