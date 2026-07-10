//! Ratatui/Crossterm terminal UI.
//!
//! Module layout:
//! - [`keymap`]: configurable key parsing and action lookup
//! - [`editor`]: the multiline comment editor widget state
//! - [`chooser`]: the base/tip target picker state and fuzzy filtering
//! - [`render`]: all drawing code (panes, popups, styles)
//!
//! This file owns the event loop, mode state machine, and event handling.

mod chooser;
mod chunks;
mod comments;
mod drafts;
mod editor;
mod flags;
mod helpers;
mod keymap;
mod ops;
mod outline;
mod render;
mod revset;
mod search;
mod tasks;
mod view_options;
mod walkthroughs;
mod zen;

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
use ratatui::{Terminal, backend::CrosstermBackend, layout::Rect};

use crate::{
    agent::{AgentProcess, ChunkPart},
    app::{Focus, ReviewSession},
    artifact::{
        ArtifactBuildOptions, ArtifactProfile, ReviewArtifact, action_item_count,
        render_handoff_markdown,
    },
    clipboard::{ClipboardMethod, copy_to_clipboard},
    config::{AgentConfig, KeybindingsConfig},
    diff::DiffSet,
    generated::GeneratedMatcher,
    jj::{JjBackend, JjChangeSummary, ReviewTarget},
    review,
    state::{ReviewState, ReviewStateTombstones, WalkthroughStep},
};
use serde_json::{Value, json};

use chooser::TargetChooserState;
use comments::CommentListState;
use drafts::DraftListState;
use editor::CommentEditor;
use flags::FlagListState;
use helpers::JjHelperState;
use keymap::{Action, KeyMap};
use ops::OperationPickerState;
use outline::SymbolOutlineState;
use render::{
    downgrade_diff_theme, draw, inner_bordered, point_in_rect, row_in_inner,
    terminal_supports_truecolor, ui_layout,
};
use revset::RevsetInputState;
use search::FileSearchState;
use tasks::TaskListState;
use view_options::ViewOptionsState;
use walkthroughs::WalkthroughListState;
use zen::ZenState;

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
    TaskList(TaskListState),
    Activity(ActivityListState),
    DraftList(DraftListState),
    FileSearch(FileSearchState),
    SymbolOutline(SymbolOutlineState),
    CommentList(CommentListState),
    ViewOptions(ViewOptionsState),
    WalkthroughList(WalkthroughListState),
    CommentInput {
        editor: CommentEditor,
        target: CommentInputTarget,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CommentInputTarget {
    New,
    Edit {
        id: String,
    },
    /// Editing an agent draft before accepting it as a comment.
    AcceptDraft {
        id: String,
    },
}

#[derive(Debug, Default)]
struct TuiState {
    launch_target: Option<ReviewTarget>,
    diff_drag: Option<DiffDrag>,
    notice: Option<UiNotice>,
    /// Fingerprint of the last autosaved files/comments payload, used to skip
    /// redundant writes between events.
    last_autosave: Option<String>,
    /// Modification time of the agent overlay at the last poll, so agent
    /// suggestions written mid-session are picked up without reloading on
    /// every tick.
    overlay_mtime: Option<std::time::SystemTime>,
    state_mtime: Option<std::time::SystemTime>,
    state_tombstones: ReviewStateTombstones,
    invalid_chunk_parts: Vec<crate::agent::InvalidChunkPart>,
    /// Where the agent overlay lives, for writing draft dispositions back.
    agent_overlay_path: Option<PathBuf>,
    /// Where a summoned agent's output is logged.
    agent_log_path: Option<PathBuf>,
    /// How to summon a review agent (from `[agent]` config).
    agent_config: AgentConfig,
    /// A summoned agent, if any. Killed on drop so quitting cannot leak it.
    agent_process: Option<AgentProcess>,
    /// This instance's registry entry; heartbeats on input, removed on drop.
    instance_registration: Option<crate::registry::InstanceRegistration>,
    /// The zen walkthrough layer, when active. A layer, not a mode: normal
    /// review actions keep working underneath it (docs/focused-diff-ux.md §6).
    zen: Option<ZenState>,
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
    /// Where a summoned agent's output is logged.
    pub agent_log: Option<PathBuf>,
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
    ignore_globs: Vec<String>,
    generated_matcher: GeneratedMatcher,
    jj: &dyn JjBackend,
    // Owned backend handed to the live ACP endpoint so agents can query the
    // stack (`review/stack_changes`, `review/change_diff`).
    acp_jj: Option<Box<dyn JjBackend + Send>>,
    paths: TuiPaths,
    agent_config: AgentConfig,
    start_tour: bool,
) -> Result<()> {
    let TuiPaths {
        state_file: state_path,
        agent_overlay: agent_overlay_path,
        acp_socket: acp_socket_path,
        agent_log: agent_log_path,
        registry_dir,
        workspace_root,
    } = paths;
    let keymap = KeyMap::try_from(keybindings)?;
    // Truecolor cue defaults quantize to indexed colors on terminals that
    // do not advertise 24-bit support (docs/focused-diff-ux.md §1).
    if !terminal_supports_truecolor() {
        downgrade_diff_theme(&mut session.diff_cues.theme);
    }
    let review_loader = ReviewLoader {
        ignore_globs,
        generated_matcher,
        jj,
    };

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
    // Render the interactive UI to stderr so stdout remains clean for artifacts.
    // This lets `gander > review.md` capture only the post-quit artifact.
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;
    let mut mode = Mode::Normal;

    // Seed the autosave fingerprint so an unchanged session does not trigger
    // a write on the first event.
    let mut tui_state = TuiState {
        launch_target: Some(session.target.clone()),
        last_autosave: Some(state_fingerprint(session)),
        agent_overlay_path: agent_overlay_path.clone(),
        state_mtime: state_path.as_deref().and_then(state_file_mtime),
        agent_log_path,
        agent_config,
        notice: acp_notice.map(|message| UiNotice {
            level: UiNoticeLevel::Info,
            message,
        }),
        ..TuiState::default()
    };
    #[cfg(unix)]
    {
        tui_state.instance_registration = instance_registration;
    }
    if tui_state.agent_config.autostart && tui_state.agent_config.command.is_some() {
        summon_agent(session, &mut tui_state);
    }
    if start_tour {
        seed_zen_tour(session, &review_loader, &mut tui_state);
        if tui_state.zen.is_none() {
            println!("nothing to tour — no walkthrough steps or changed files in this target");
            return Ok(());
        }
    }
    let result = run_loop(
        &mut terminal,
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

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    result
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
    let matcher = GeneratedMatcher::new(&Default::default())?;
    let loader = ReviewLoader {
        ignore_globs: Vec::new(),
        generated_matcher: matcher,
        jj,
    };
    let mut tui_state = TuiState::default();
    seed_zen_tour(session, &loader, &mut tui_state);
    let Some(mut zen) = tui_state.zen.clone() else {
        return Ok(
            "nothing to tour — no walkthrough steps or changed files in this target\n".to_owned(),
        );
    };
    let total = zen.stops.len() + usize::from(zen.has_glance());
    let indices: Vec<usize> = match slide {
        Some(n) => vec![n.saturating_sub(1).min(total.saturating_sub(1))],
        None => (0..total).collect(),
    };
    let mut out = String::new();
    for idx in indices {
        if idx < zen.stops.len() {
            zen.index = idx;
            zen.phase = zen::ZenPhase::Focus;
            if let Some(stop) = zen.current().cloned() {
                let _ = zen_goto_stop(&loader, session, &mut zen, &stop, &mut tui_state);
            }
        } else {
            zen.phase = zen::ZenPhase::Glance;
        }
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| {
            render::draw(
                frame,
                session,
                &Mode::Normal,
                &keymap,
                &tui_state,
                None,
                Some(&zen),
            )
        })?;
        let breadcrumb = if idx < zen.stops.len() {
            zen.current().map(tour_breadcrumb).unwrap_or_default()
        } else {
            "at a glance".to_owned()
        };
        out.push_str(&format!("──── slide {}/{} ────\n", idx + 1, total));
        let mut slide_text = tour_buffer_text(
            terminal.backend().buffer(),
            &format!("slide {}/{} · {breadcrumb}", idx + 1, total),
        );
        slide_text = slide_text
            .replace(" — j/k", "")
            .replace("j/k select · enter dives to location · esc ends tour", "")
            .replace(
                "j/k move · enter jump · a mark all viewed & finish · p back · esc end",
                "",
            );
        out.push_str(&slide_text);
        if let Some(stop) = zen.current() {
            for artifact in zen::stop_artifacts(stop) {
                out.push_str(&format!("\n  exhibit: {}\n", artifact.title));
                for line in artifact.body.lines() {
                    out.push_str("  ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
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

fn tour_breadcrumb(stop: &zen::ZenStop) -> String {
    match stop {
        zen::ZenStop::Chapter(chapter) => chapter
            .change_id
            .as_ref()
            .map(|id| format!("change {id}"))
            .unwrap_or_else(|| "chapter".to_owned()),
        zen::ZenStop::Chunk(row) => row
            .part
            .as_ref()
            .map(|part| match (part.start_line, part.end_line) {
                (Some(start), Some(end)) => format!("{}:{start}-{end}", part.path),
                (Some(start), None) => format!("{}:{start}", part.path),
                _ => part.path.clone(),
            })
            .unwrap_or_else(|| row.title.clone()),
    }
}

fn seed_zen_tour(
    session: &mut ReviewSession,
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) {
    let mut stack = review_loader
        .jj
        .stack_changes(&session.repo, &session.target)
        .unwrap_or_default();
    stack.retain(|change| !change.matches_rev(&session.target.base));
    load_change_diffs_for_stack(review_loader, session, &stack);
    match ZenState::new(session, &stack) {
        Some(mut zen) => {
            session.file_pane_visible = false;
            if let Some(stop) = zen.current().cloned() {
                let _ = zen_goto_stop(review_loader, session, &mut zen, &stop, tui_state);
            }
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: match zen.source {
                    zen::ZenSource::Curated => format!(
                        "zen: {} chapter(s), {} focus stop(s), {} at a glance",
                        zen.chapter_count(),
                        zen.chunk_stop_count(),
                        zen.glance_rows.len()
                    ),
                    zen::ZenSource::Files => format!(
                        "zen: touring {} file(s) — summon an agent (@) to curate focus stops",
                        zen.chunk_stop_count()
                    ),
                },
            });
            tui_state.zen = Some(zen);
        }
        None => {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: "nothing to review — no changed files in this target".to_owned(),
            })
        }
    }
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
        // Answer queued agent requests against the live session before
        // drawing so their effects render this frame.
        #[cfg(unix)]
        if let Some(bridge) = acp_bridge.as_deref_mut() {
            let (overlay_changed, commands) = bridge.drain_ui_commands(session);
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

        // A retarget (t/p/b/R, stack step, operation picker) invalidates the
        // walkthrough stops; end zen rather than touring a stale map.
        if tui_state
            .zen
            .as_ref()
            .is_some_and(|zen| zen.is_stale(session))
        {
            let zen = tui_state.zen.take().expect("checked above");
            zen::end(session, &zen);
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: "zen ended — review target changed".to_owned(),
            });
        }

        terminal.draw(|frame| {
            draw(
                frame,
                session,
                mode,
                keymap,
                tui_state,
                tui_state.notice.as_ref(),
                tui_state.zen.as_ref(),
            )
        })?;

        if !event::poll(Duration::from_millis(150))? {
            // Idle ticks are the natural moment to pick up agent overlay
            // writes without competing with user input handling.
            if let Some(overlay_path) = agent_overlay_path {
                maybe_reload_agent_overlay(session, overlay_path, tui_state, review_loader, true);
            }
            if let Some(state_path) = state_path {
                maybe_reload_review_state(session, state_path, tui_state, true);
            }
            notice_agent_exit(tui_state);
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

        match event::read()? {
            Event::Key(key)
                if handle_key_event(key, session, mode, keymap, review_loader, tui_state)? =>
            {
                if let Some(state_path) = state_path {
                    autosave_state(session, state_path, tui_state);
                }
                break;
            }
            Event::Key(_) => {}
            Event::Mouse(mouse) => {
                handle_mouse_event(mouse, terminal.size()?, session, mode, tui_state)
            }
            _ => {}
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
    }
    Ok(())
}

/// Launch the configured review agent (`[agent] command`), agent-agnostic:
/// the command is any CLI that accepts a prompt. Output is logged to the
/// workspace's agent log (see `gander paths`); suggestions arrive through
/// ACP like any other agent.
fn summon_agent(session: &ReviewSession, tui_state: &mut TuiState) {
    if let Some(process) = &mut tui_state.agent_process
        && process.try_status().is_none()
    {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Info,
            message: "agent already running".to_owned(),
        });
        return;
    }
    let Some(command) = tui_state.agent_config.command.clone() else {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Info,
            message: "no [agent] command configured (see gander.toml)".to_owned(),
        });
        return;
    };
    let Some(log_path) = tui_state.agent_log_path.clone() else {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Error,
            message: "no agent log path resolved; cannot summon an agent".to_owned(),
        });
        return;
    };
    let prompt = crate::agent::review_prompt(
        tui_state.agent_config.prompt.as_deref(),
        &session.repo,
        &session.target.base,
        &session.target.rev,
    );
    match AgentProcess::spawn(&session.repo, &command, &prompt, &log_path) {
        Ok(process) => {
            tui_state.agent_process = Some(process);
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: format!("agent summoned (output: {})", log_path.display()),
            });
        }
        Err(error) => {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: format!("failed to summon agent: {error}"),
            });
        }
    }
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
    match render_handoff_markdown(session, options).and_then(|body| copy(&body)) {
        Ok(method) => {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: format!("handoff copied via {method} ({count} action items)"),
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

/// Announce a summoned agent's exit exactly once and release the handle so
/// it can be summoned again.
fn notice_agent_exit(tui_state: &mut TuiState) {
    if let Some(process) = &mut tui_state.agent_process
        && let Some(status) = process.try_status()
    {
        tui_state.agent_process = None;
        tui_state.notice = Some(if status.success() {
            UiNotice {
                level: UiNoticeLevel::Info,
                message: "agent finished its review".to_owned(),
            }
        } else {
            UiNotice {
                level: UiNoticeLevel::Error,
                message: format!("agent exited with {status} (see the agent log: gander paths)"),
            }
        });
    }
}

/// Reload the agent overlay when its mtime changes, applying suggestions to
/// the session. `notify` controls whether a footer notice announces updates
/// (suppressed for the initial load).
fn maybe_reload_agent_overlay(
    session: &mut ReviewSession,
    overlay_path: &Path,
    tui_state: &mut TuiState,
    review_loader: &ReviewLoader<'_>,
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
            let (overlay, invalid) = validated_overlay_for_tui(session, review_loader, overlay);
            session.apply_agent_overlay(&overlay);
            tui_state.invalid_chunk_parts = invalid.clone();
            if notify {
                let message = if let Some(first) = invalid.first() {
                    format!(
                        "agent overlay: {} invalid chunk part(s) ignored — {}",
                        invalid.len(),
                        first.reason
                    )
                } else {
                    "agent suggestions updated".to_owned()
                };
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message,
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
            let before_comments: BTreeSet<String> = session
                .comments
                .iter()
                .map(|comment| comment.id.clone())
                .collect();
            let mut merged = session.to_state();
            merged.merge_external(external, &tui_state.state_tombstones);
            let added_comments = merged
                .comments
                .iter()
                .filter(|comment| !before_comments.contains(&comment.id))
                .count();
            session.apply_review_state(merged);
            tui_state.state_mtime = mtime;
            tui_state.last_autosave = Some(state_fingerprint(session));
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

fn validated_overlay_for_tui(
    session: &ReviewSession,
    review_loader: &ReviewLoader<'_>,
    mut overlay: crate::agent::AgentOverlay,
) -> (
    crate::agent::AgentOverlay,
    Vec<crate::agent::InvalidChunkPart>,
) {
    let session_files = session
        .files
        .iter()
        .map(|file| file.diff.clone())
        .collect::<Vec<_>>();
    let mut parsed_changes = Vec::new();
    for change_id in overlay
        .chunks
        .iter()
        .filter_map(|chunk| chunk.change_id.as_ref())
    {
        if parsed_changes
            .iter()
            .any(|(existing, _): &(String, crate::diff::DiffSet)| existing == change_id)
        {
            continue;
        }
        let target = ReviewTarget::new(format!("{change_id}-"), change_id.clone());
        if let Ok(raw) = review_loader.jj.diff(&session.repo, &target)
            && let Ok(diff) = DiffSet::parse(&raw)
        {
            parsed_changes.push((change_id.clone(), diff));
        }
    }
    let change_diffs = parsed_changes
        .iter()
        .map(|(change_id, diff)| crate::agent::ChangeDiffContext {
            change_id: change_id.clone(),
            files: &diff.files,
        })
        .collect::<Vec<_>>();
    let invalid = crate::agent::validate_review_chunks(
        &overlay.chunks,
        &crate::agent::ChunkValidationContext {
            session_files: &session_files,
            change_diffs,
        },
    );
    if !invalid.is_empty() {
        overlay.chunks = crate::agent::remove_invalid_chunk_parts(&overlay.chunks, &invalid);
    }
    (overlay, invalid)
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
        Mode::TaskList(_) => "task list",
        Mode::Activity(_) => "activity",
        Mode::DraftList(_) => "draft list",
        Mode::FileSearch(_) => "file search",
        Mode::SymbolOutline(_) => "symbol outline",
        Mode::CommentList(_) => "comment list",
        Mode::ViewOptions(_) => "view options",
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
            start_present_tour(review_loader, session, tui_state)?;
            Ok(present_status(session, tui_state))
        }
        PresentCommand::End => {
            if let Some(zen) = tui_state.zen.take() {
                zen::end(session, &zen);
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "zen ended by presenter".to_owned(),
                });
            }
            Ok(present_status(session, tui_state))
        }
        PresentCommand::Next => {
            let Some(mut zen) = tui_state.zen.take() else {
                return Err((-32002, "tour is not active".to_owned()));
            };
            if zen.advance()
                && let Some(stop) = zen.current().cloned()
            {
                zen_goto_stop(review_loader, session, &mut zen, &stop, tui_state);
            }
            tui_state.zen = Some(zen);
            Ok(present_status(session, tui_state))
        }
        PresentCommand::Prev => {
            let Some(mut zen) = tui_state.zen.take() else {
                return Err((-32002, "tour is not active".to_owned()));
            };
            if zen.back()
                && let Some(stop) = zen.current().cloned()
            {
                zen_goto_stop(review_loader, session, &mut zen, &stop, tui_state);
            }
            tui_state.zen = Some(zen);
            Ok(present_status(session, tui_state))
        }
        PresentCommand::GotoIndex(index) => {
            let Some(mut zen) = tui_state.zen.take() else {
                return Err((-32002, "tour is not active".to_owned()));
            };
            if index >= zen.stops.len() {
                return Err((-32602, format!("slide index {index} out of range")));
            }
            zen.index = index;
            if let Some(stop) = zen.current().cloned() {
                zen_goto_stop(review_loader, session, &mut zen, &stop, tui_state);
            }
            tui_state.zen = Some(zen);
            Ok(present_status(session, tui_state))
        }
        PresentCommand::GotoStep(step_id) => {
            let Some(mut zen) = tui_state.zen.take() else {
                return Err((-32002, "tour is not active".to_owned()));
            };
            let Some(index) = zen.stops.iter().position(|stop| match stop {
                zen::ZenStop::Chunk(row) => row.source_id == step_id,
                zen::ZenStop::Chapter(_) => false,
            }) else {
                tui_state.zen = Some(zen);
                return Err((-32602, format!("unknown step_id: {step_id}")));
            };
            zen.index = index;
            if let Some(stop) = zen.current().cloned() {
                zen_goto_stop(review_loader, session, &mut zen, &stop, tui_state);
            }
            tui_state.zen = Some(zen);
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
            session.jump_to_chunk_part(&ChunkPart {
                path: path.clone(),
                start_line: Some(line),
                end_line,
            });
            session.focus = Focus::Diff;
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
            if tui_state.zen.is_some() {
                reload_present_tour(review_loader, session, tui_state)?;
            }
            Ok(present_status(session, tui_state))
        }
    }
}

fn present_status(session: &ReviewSession, tui_state: &TuiState) -> Value {
    let Some(zen) = tui_state.zen.as_ref() else {
        return json!({ "active": false });
    };
    json!({
        "active": true,
        "slide_index": zen.index,
        "slide_count": zen.stops.len(),
        "phase": format!("{:?}", zen.phase).to_lowercase(),
        "current": current_present_stop(session, zen),
    })
}

fn current_present_stop(session: &ReviewSession, zen: &ZenState) -> Value {
    match zen.current() {
        Some(zen::ZenStop::Chapter(chapter)) => {
            json!({ "title": chapter.title(), "path": null, "line": null })
        }
        Some(zen::ZenStop::Chunk(row)) => {
            let (path, line) = row
                .part
                .as_ref()
                .map(|p| (Some(p.path.as_str()), p.start_line))
                .unwrap_or((None, None));
            json!({ "title": row.title, "path": path, "line": line })
        }
        None => json!({ "title": session.summary_line(), "path": null, "line": null }),
    }
}

fn start_present_tour(
    review_loader: &ReviewLoader<'_>,
    session: &mut ReviewSession,
    tui_state: &mut TuiState,
) -> Result<(), (i64, String)> {
    if tui_state.zen.is_some() {
        return Ok(());
    }
    let mut stack = review_loader
        .jj
        .stack_changes(&session.repo, &session.target)
        .unwrap_or_default();
    stack.retain(|change| !change.matches_rev(&session.target.base));
    load_change_diffs_for_stack(review_loader, session, &stack);
    let Some(mut zen) = ZenState::new(session, &stack) else {
        return Err((
            -32002,
            "nothing to review — no changed files in this target".to_owned(),
        ));
    };
    session.file_pane_visible = false;
    if let Some(stop) = zen.current().cloned() {
        zen_goto_stop(review_loader, session, &mut zen, &stop, tui_state);
    }
    tui_state.zen = Some(zen);
    Ok(())
}

fn reload_present_tour(
    review_loader: &ReviewLoader<'_>,
    session: &mut ReviewSession,
    tui_state: &mut TuiState,
) -> Result<(), (i64, String)> {
    let mut stack = review_loader
        .jj
        .stack_changes(&session.repo, &session.target)
        .unwrap_or_default();
    stack.retain(|change| !change.matches_rev(&session.target.base));
    load_change_diffs_for_stack(review_loader, session, &stack);
    let Some(zen) = tui_state.zen.as_mut() else {
        return start_present_tour(review_loader, session, tui_state);
    };
    if !zen.refresh(session, &stack) {
        tui_state.zen = None;
        return Err((-32002, "tour has no slides after reload".to_owned()));
    }
    if let Some(mut zen) = tui_state.zen.take() {
        if let Some(stop) = zen.current().cloned() {
            zen_goto_stop(review_loader, session, &mut zen, &stop, tui_state);
        }
        tui_state.zen = Some(zen);
    }
    Ok(())
}

/// Reload the current target in place: view state survives, agent
/// suggestions are reapplied, and an active zen walkthrough rebuilds its
/// stops instead of going stale.
fn refresh_current_target(
    review_loader: &ReviewLoader<'_>,
    session: &mut ReviewSession,
    tui_state: &mut TuiState,
    previous_fingerprint: &str,
    fingerprint: &str,
) {
    if let Err(error) = review_loader.load_in_place(session, session.target.clone()) {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Error,
            message: format!("failed to refresh review: {error:?}"),
        });
        return;
    }
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
    if let Some(mut zen) = tui_state.zen.take() {
        let mut stack = review_loader
            .jj
            .stack_changes(&session.repo, &session.target)
            .unwrap_or_default();
        stack.retain(|change| !change.matches_rev(&session.target.base));
        load_change_diffs_for_stack(review_loader, session, &stack);
        if zen.refresh(session, &stack) {
            if zen.phase != zen::ZenPhase::Glance
                && let Some(stop) = zen.current().cloned()
            {
                zen::jump_to_stop(session, &stop);
            }
            tui_state.zen = Some(zen);
        } else {
            zen::end(session, &zen);
            message.push_str(" · zen ended (nothing left to walk through)");
        }
    }
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
/// reset overlay-derived state (ordering, flags, chunks, drafts); refreshes
/// and zen-driven retargets restore it so suggestions survive.
fn reapply_agent_overlay(
    session: &mut ReviewSession,
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) {
    let Some(overlay_path) = tui_state.agent_overlay_path.clone() else {
        return;
    };
    if let Ok(overlay) = crate::agent::AgentOverlay::load_or_default(&overlay_path) {
        let (overlay, invalid) = validated_overlay_for_tui(session, review_loader, overlay);
        if !invalid.is_empty() {
            let message = invalid_chunk_notice(invalid.len());
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: message.clone(),
            });
            push_activity(tui_state, "agent-overlay-invalid".to_owned(), message);
        }
        tui_state.invalid_chunk_parts = invalid;
        session.apply_agent_overlay(&overlay);
    }
    // Our own read is not news; suppress the poll-based reload notice.
    tui_state.overlay_mtime = std::fs::metadata(&overlay_path)
        .and_then(|metadata| metadata.modified())
        .ok();
}

fn invalid_chunk_notice(count: usize) -> String {
    format!(
        "{count} curated walkthrough part(s) no longer match the diff; update the agent overlay or durable walkthrough targets"
    )
}

/// Cheap change-detection payload: only the persistable parts of the session
/// (viewed marks and comments), excluding volatile metadata like `saved_at`.
fn state_fingerprint(session: &ReviewSession) -> String {
    let state = session.to_state();
    serde_json::to_string(&(&state.files, &state.comments)).unwrap_or_default()
}

fn autosave_state(session: &mut ReviewSession, state_path: &Path, tui_state: &mut TuiState) {
    let fingerprint = state_fingerprint(session);
    let disk_mtime = state_file_mtime(state_path);
    if tui_state.last_autosave.as_deref() == Some(fingerprint.as_str())
        && disk_mtime == tui_state.state_mtime
    {
        return;
    }
    let mut state = session.to_state();
    if disk_mtime != tui_state.state_mtime
        && let Ok(on_disk) = crate::state::ReviewState::load_or_default(state_path)
    {
        state.merge_external(on_disk, &tui_state.state_tombstones);
        session.apply_review_state(state.clone());
    }
    match state.save(state_path) {
        Ok(()) => {
            tui_state.state_mtime = state_file_mtime(state_path);
            tui_state.last_autosave = Some(state_fingerprint(session));
        }
        Err(error) => {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: format!("failed to autosave review state: {error}"),
            });
        }
    }
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
            // The zen layer intercepts stop-navigation keys and lets
            // everything else fall through to the normal vocabulary, so
            // commenting/flagging/view toggles keep working mid-walkthrough.
            if let Some(mut zen) = tui_state.zen.take() {
                match handle_zen_key(key, &mut zen, session, keymap, review_loader, tui_state) {
                    ZenKeyOutcome::Consumed => {
                        tui_state.zen = Some(zen);
                        return Ok(false);
                    }
                    ZenKeyOutcome::End { restore_target } => {
                        // A change-anchored walkthrough may have wandered
                        // through the stack; ending it returns to the target
                        // it started from (unless the human jumped somewhere
                        // on purpose).
                        if restore_target && session.target != zen.home_target {
                            match review_loader.load(session, zen.home_target.clone()) {
                                Ok(()) => {
                                    reapply_agent_overlay(session, review_loader, tui_state);
                                    zen::mark_glance_viewed(session, &zen);
                                }
                                Err(error) => {
                                    tui_state.notice = Some(UiNotice {
                                        level: UiNoticeLevel::Error,
                                        message: format!(
                                            "zen ended but failed to restore {}: {error:?}",
                                            zen.home_target
                                        ),
                                    });
                                }
                            }
                        }
                        zen::end(session, &zen);
                        return Ok(false);
                    }
                    ZenKeyOutcome::Fallthrough => {
                        tui_state.zen = Some(zen);
                    }
                }
            }
            if let Some(action) = keymap.action_for(&key)
                && handle_normal_action(action, session, mode, review_loader, tui_state)?
            {
                return Ok(true);
            }
        }
        Mode::Help => {
            // Any key dismisses the help overlay; it is purely informational.
            *mode = Mode::Normal;
        }
        Mode::TargetChooser(chooser) => {
            if handle_target_chooser_key(key, chooser, session, keymap, review_loader, tui_state) {
                *mode = Mode::Normal;
            }
        }
        Mode::RevsetInput(input) => {
            if handle_revset_input_key(key, input, session, review_loader, tui_state) {
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
            if handle_flag_list_key(key, list, session, keymap) {
                *mode = Mode::Normal;
            }
        }
        Mode::TaskList(list) => {
            if handle_task_list_key(key, list, session, keymap, tui_state) {
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
            if handle_file_search_key(key, search, session, keymap) {
                *mode = Mode::Normal;
            }
        }
        Mode::SymbolOutline(outline) => {
            if handle_symbol_outline_key(key, outline, session, keymap) {
                *mode = Mode::Normal;
            }
        }
        Mode::CommentList(list) => {
            if handle_comment_list_key(key, list, session, keymap, tui_state) {
                *mode = Mode::Normal;
            }
        }
        Mode::ViewOptions(state) => {
            if handle_view_options_key(key, state, session, keymap) {
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
    Ok(false)
}

fn handle_normal_action(
    action: Action,
    session: &mut ReviewSession,
    mode: &mut Mode,
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) -> Result<bool> {
    match action {
        Action::Quit => return Ok(true),
        Action::Help => *mode = Mode::Help,
        Action::SummonAgent => summon_agent(session, tui_state),
        Action::YankHandoff => yank_handoff(session, tui_state),
        Action::MoveDown => match session.focus {
            Focus::Files => session.move_selection(1),
            Focus::Diff => session.move_diff_cursor(1),
        },
        Action::MoveUp => match session.focus {
            Focus::Files => session.move_selection(-1),
            Focus::Diff => session.move_diff_cursor(-1),
        },
        Action::ToggleFocus => session.toggle_focus(),
        Action::DiffTop => session.diff_scroll = 0,
        Action::DiffBottom => session.scroll_diff_to_bottom(),
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
        Action::TaskList => {
            let tasks = TaskListState::new(session);
            if tasks.comment_ids.is_empty() {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no review tasks".to_owned(),
                });
            } else {
                *mode = Mode::TaskList(tasks);
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
        Action::Zen => {
            seed_zen_tour(session, review_loader, tui_state);
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
        Action::NextSymbol => session.jump_to_changed_symbol(1),
        Action::PreviousSymbol => session.jump_to_changed_symbol(-1),
        Action::NextChangedHunk => session.jump_to_changed_hunk(1),
        Action::PreviousChangedHunk => session.jump_to_changed_hunk(-1),
        Action::CommentList => {
            if session.comments.is_empty() {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no comments recorded yet".to_owned(),
                });
            } else {
                *mode = Mode::CommentList(CommentListState::default());
            }
        }
        Action::NextComment => session.move_to_comment(1),
        Action::PreviousComment => session.move_to_comment(-1),
        Action::ScrollDown => session.scroll_diff(12),
        Action::ScrollUp => session.scroll_diff(-12),
        Action::MarkViewed => session.mark_selected_viewed(),
        Action::ToggleViewed => session.toggle_viewed(),
        Action::MarkAllViewed => session.mark_all_viewed(),
        Action::ToggleGenerated => session.toggle_generated_visibility(),
        Action::CycleViewedFilter => session.cycle_viewed_filter(),
        Action::ToggleFold => {
            if session.focus == Focus::Files {
                session.toggle_tree_fold();
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
        Action::ToggleContextFold => session.toggle_context_fold(),
        Action::ExpandContext => {
            let step = session.diff_cues.context_step;
            expand_diff_context(session, review_loader, tui_state, Some(step));
        }
        Action::ExpandContextAll => expand_diff_context(session, review_loader, tui_state, None),
        Action::CollapseContext => {
            if !session.collapse_nearest_gap() {
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
        Action::ToggleFilePane => session.toggle_file_pane(),
        Action::ToggleDiffView => session.toggle_diff_view(),
        Action::ToggleLargeDiff => session.toggle_large_diff_render(),
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
            // Esc-style dismissal: clear the transient layers (range selection
            // and footer notice) instead of quitting.
            session.clear_diff_range_selection();
            tui_state.notice = None;
        }
        Action::Comment => {
            *mode = Mode::CommentInput {
                editor: CommentEditor::default(),
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
                    editor: CommentEditor {
                        text: comment.body.clone(),
                        cursor: comment.body.len(),
                    },
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
        | Action::TargetPickerMoveDown
        | Action::TargetPickerMoveUp => {}
    }
    Ok(false)
}

fn ensure_tui_review_session(session: &mut ReviewSession) -> &mut crate::state::ReviewSession {
    let mut state = ReviewState {
        sessions: std::mem::take(&mut session.sessions),
        ..ReviewState::default()
    };
    let spec = review::SessionTargetSpec {
        repo: Some(session.repo.display().to_string()),
        base: Some(session.target.base.clone()),
        revision: Some(session.target.rev.clone()),
        revset: None,
    };
    let id = review::ensure_session(&mut state, &spec, None).id.clone();
    session.sessions = state.sessions;
    session.sessions.iter_mut().find(|s| s.id == id).unwrap()
}

fn walkthrough_target_from_selection(
    session: &ReviewSession,
) -> Option<crate::state::ReviewTarget> {
    let anchor = session
        .selected_range_anchor()
        .or_else(|| session.selected_line_anchor())?;
    let line = anchor.line()?;
    Some(crate::state::ReviewTarget {
        repo: Some(session.repo.display().to_string()),
        base: Some(session.target.base.clone()),
        revision: Some(session.target.rev.clone()),
        file: Some(anchor.path().to_owned()),
        line: Some(line),
        end_line: anchor.end_line().filter(|end| *end != line),
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
    let durable = ensure_tui_review_session(session);
    let step = review::add_walkthrough_step(
        durable,
        WalkthroughStep {
            target,
            title: Some(title.clone()),
            ..Default::default()
        },
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
    match review_loader.load(session, target) {
        Ok(()) => {
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
    if let Some(action) = keymap.target_picker_action_for(&key) {
        match action {
            Action::TargetPickerMoveDown => chooser.move_selection(1),
            Action::TargetPickerMoveUp => chooser.move_selection(-1),
            _ => {}
        }
        return false;
    }

    match key.code {
        KeyCode::Esc => true,
        KeyCode::Enter => {
            if let Some(target) = chooser.target() {
                load_review_target(review_loader, session, target, tui_state);
            }
            true
        }
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
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) -> bool {
    match key.code {
        KeyCode::Esc => true,
        KeyCode::Enter => match input.target() {
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
        },
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
    if let Some(action) = keymap.action_for(&key) {
        match action {
            Action::MoveDown => {
                picker.move_selection(1);
                refresh_operation_picker_preview(picker, session, review_loader);
                return false;
            }
            Action::MoveUp => {
                picker.move_selection(-1);
                refresh_operation_picker_preview(picker, session, review_loader);
                return false;
            }
            _ => {}
        }
    }

    if let Some(action) = keymap.target_picker_action_for(&key) {
        match action {
            Action::TargetPickerMoveDown => {
                picker.move_selection(1);
                refresh_operation_picker_preview(picker, session, review_loader);
            }
            Action::TargetPickerMoveUp => {
                picker.move_selection(-1);
                refresh_operation_picker_preview(picker, session, review_loader);
            }
            _ => {}
        }
        return false;
    }

    match key.code {
        KeyCode::Esc => true,
        KeyCode::Enter => {
            if let Some(operation) = picker.selected_operation().cloned() {
                apply_incremental_review(review_loader, session, &operation, tui_state);
            }
            true
        }
        _ => false,
    }
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
    let target = session.target.clone();
    if let Err(error) = review_loader.load_in_place(session, target.clone()) {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Error,
            message: format!(
                "failed to refresh {target} before prior-operation compare: {error:?}"
            ),
        });
        return;
    }
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
            return;
        }
    };
    let (caught_up, already_viewed, changed) =
        session.apply_incremental_review(&prior_fingerprints);
    tui_state.notice = Some(UiNotice {
        level: UiNoticeLevel::Info,
        message: format!(
            "{caught_up} file(s) caught up (unchanged since {}), {already_viewed} already viewed; {changed} changed/new file(s) need re-review",
            operation.operation_id
        ),
    });
}

fn handle_jj_helpers_key(
    key: KeyEvent,
    state: &mut JjHelperState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) -> bool {
    if let Some(action) = keymap.target_picker_action_for(&key) {
        match action {
            Action::TargetPickerMoveDown => state.move_selection(1),
            Action::TargetPickerMoveUp => state.move_selection(-1),
            _ => {}
        }
        return false;
    }

    match key.code {
        // Esc dismisses one layer: the confirmation first, then the popup.
        KeyCode::Esc => {
            if state.confirming {
                state.confirming = false;
                false
            } else {
                true
            }
        }
        KeyCode::Enter => {
            if state.selected_option().is_none() {
                return true;
            }
            if !state.confirming {
                state.confirming = true;
                return false;
            }
            let option = state.selected_option().cloned().expect("checked above");
            run_jj_helper(review_loader, session, &option, tui_state);
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            state.move_selection(1);
            false
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.move_selection(-1);
            false
        }
        _ => false,
    }
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
            let reload = review_loader.load(session, session.target.clone());
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
) -> bool {
    // The popup's own binding closes it, so `V` toggles the popup.
    if keymap.action_for(&key) == Some(Action::ViewOptions) {
        return true;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => true,
        KeyCode::Enter | KeyCode::Char(' ') => {
            state.selected_option().toggle(session);
            false
        }
        KeyCode::Char('j') | KeyCode::Down => {
            state.move_selection(1);
            false
        }
        KeyCode::Char('k') | KeyCode::Up => {
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
) -> bool {
    if let Some(action) = keymap.target_picker_action_for(&key) {
        match action {
            Action::TargetPickerMoveDown => list.move_selection(1),
            Action::TargetPickerMoveUp => list.move_selection(-1),
            _ => {}
        }
        return false;
    }

    match key.code {
        KeyCode::Esc => true,
        KeyCode::Enter => {
            if let Some(flag) = list.selected_flag().cloned() {
                session.jump_to_flag(&flag);
            }
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            list.move_selection(1);
            false
        }
        KeyCode::Char('k') | KeyCode::Up => {
            list.move_selection(-1);
            false
        }
        _ => false,
    }
}

/// What the zen layer decided about a key press.
enum ZenKeyOutcome {
    /// The key was a zen navigation key and has been handled.
    Consumed,
    /// The walkthrough is over; the caller clears the layer.
    /// `restore_target` asks the caller to return to the walkthrough's home
    /// target when change-anchored stops wandered through the stack; it is
    /// `false` when the human deliberately jumped somewhere instead.
    End { restore_target: bool },
    /// Not a zen key: let the normal-mode vocabulary handle it.
    Fallthrough,
}

/// Bring the session to a zen stop, retargeting the review when the stop is
/// anchored to a different jj change than the one loaded (the stacked-PR
/// walkthrough). Returns `false` when the retarget failed: a notice
/// explains, and the caller should stay on its current stop.
fn zen_goto_stop(
    review_loader: &ReviewLoader<'_>,
    session: &mut ReviewSession,
    zen: &mut ZenState,
    stop: &zen::ZenStop,
    tui_state: &mut TuiState,
) -> bool {
    let desired = zen::stop_target(stop, &zen.home_target);
    if session.target != desired {
        if let Err(error) = review_loader.load(session, desired.clone()) {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: format!("failed to load {desired}: {error:?}"),
            });
            return false;
        }
        // A zen-driven load is not a user retarget: keep the walkthrough
        // alive (staleness key), its chrome (hidden file pane), and the
        // agent's suggestions (the loader reset all three).
        zen.target_key = session.target.to_string();
        session.file_pane_visible = false;
        reapply_agent_overlay(session, review_loader, tui_state);
    }
    zen::jump_to_stop(session, stop);
    true
}

/// Zen layer keys, phase-aware. On the focus card and reading view:
/// enter/n/→ advance (marking the current stop's file viewed; past the last
/// stop the glance board opens), p/← step back, tab/o toggle the focus card
/// against the dimmed reading view, g opens the glance board, esc (or the
/// zen key) ends the walkthrough. Everything else falls through to the
/// normal keymap so the full review vocabulary (comments, flags, context
/// expansion, view toggles) keeps working mid-walkthrough. The glance board
/// captures navigation keys itself: j/k move, enter jumps and ends zen,
/// a bulk-marks every glance file viewed and finishes.
fn handle_zen_key(
    key: KeyEvent,
    zen: &mut ZenState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) -> ZenKeyOutcome {
    if zen.phase == zen::ZenPhase::Glance {
        return handle_zen_glance_key(key, zen, session, keymap, review_loader, tui_state);
    }
    if let zen::ZenPhase::Artifact { index, scroll } = zen.phase {
        return handle_zen_artifact_key(key, zen, index, scroll);
    }
    match key.code {
        KeyCode::Esc => {
            // Esc peels layers in order: an active range selection is more
            // transient than the walkthrough, so cancel it first.
            if session.has_active_diff_range() {
                return ZenKeyOutcome::Fallthrough;
            }
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: "zen ended".to_owned(),
            });
            return ZenKeyOutcome::End {
                restore_target: true,
            };
        }
        KeyCode::Enter | KeyCode::Char('n') | KeyCode::Right | KeyCode::Char(' ') => {
            let Some(stop) = zen.current().cloned() else {
                return ZenKeyOutcome::End {
                    restore_target: true,
                };
            };
            zen::mark_stop_viewed(session, &stop);
            if zen.advance() {
                if let Some(next) = zen.current().cloned()
                    && !zen_goto_stop(review_loader, session, zen, &next, tui_state)
                {
                    // The next stop's change failed to load: stay put
                    // rather than showing a card over the wrong diff.
                    zen.back();
                    return ZenKeyOutcome::Consumed;
                }
                tui_state.notice = None;
                return ZenKeyOutcome::Consumed;
            }
            // Past the last spotlight: the glance board finishes the
            // briefing, so the boilerplate is skimmed rather than skipped.
            if zen.has_glance() {
                zen.phase = zen::ZenPhase::Glance;
                tui_state.notice = None;
                return ZenKeyOutcome::Consumed;
            }
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: "zen complete — all stops visited".to_owned(),
            });
            return ZenKeyOutcome::End {
                restore_target: true,
            };
        }
        KeyCode::Char('p') | KeyCode::Left => {
            if zen.back()
                && let Some(stop) = zen.current().cloned()
                && !zen_goto_stop(review_loader, session, zen, &stop, tui_state)
            {
                zen.advance();
            }
            return ZenKeyOutcome::Consumed;
        }
        KeyCode::Tab => {
            zen.phase = match zen.phase {
                zen::ZenPhase::Focus => zen::ZenPhase::Reading,
                _ => zen::ZenPhase::Focus,
            };
            return ZenKeyOutcome::Consumed;
        }
        KeyCode::Char('.') => {
            // Refocus: snap the cursor/scroll back to the current stop after
            // wandering off it with line navigation.
            if let Some(stop) = zen.current().cloned() {
                zen_goto_stop(review_loader, session, zen, &stop, tui_state);
            }
            return ZenKeyOutcome::Consumed;
        }
        KeyCode::Char('g') if zen.phase == zen::ZenPhase::Focus => {
            if zen.has_glance() {
                zen.phase = zen::ZenPhase::Glance;
            } else {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "nothing on the glance board — every change is a stop".to_owned(),
                });
            }
            return ZenKeyOutcome::Consumed;
        }
        KeyCode::Char('e') if zen.phase == zen::ZenPhase::Focus => {
            let artifacts = zen
                .current()
                .map(|stop| zen::stop_artifacts(stop).len())
                .unwrap_or(0);
            if artifacts > 0 {
                zen.phase = zen::ZenPhase::Artifact {
                    index: 0,
                    scroll: 0,
                };
            } else {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no artifacts on this stop".to_owned(),
                });
            }
            return ZenKeyOutcome::Consumed;
        }
        KeyCode::Char('d')
            if zen.phase == zen::ZenPhase::Focus
                && matches!(zen.current(), Some(zen::ZenStop::Chapter(_))) =>
        {
            zen.chapter_description_collapsed = !zen.chapter_description_collapsed;
            zen.chapter_brief_expanded = !zen.chapter_brief_expanded;
            return ZenKeyOutcome::Consumed;
        }
        _ => {}
    }
    // Pressing the zen key again also ends the walkthrough.
    if keymap.action_for(&key) == Some(Action::Zen) {
        tui_state.notice = Some(UiNotice {
            level: UiNoticeLevel::Info,
            message: "zen ended".to_owned(),
        });
        return ZenKeyOutcome::End {
            restore_target: true,
        };
    }
    ZenKeyOutcome::Fallthrough
}

/// Glance board keys. Unlike the focus/reading surfaces the board captures
/// everything (it is a bulk-skim screen, not a diff view): j/k/↑/↓ move,
/// enter jumps to the selected entry in the normal UI and ends zen, `a`
/// marks every glance file viewed and finishes, p/← returns to the last
/// spotlight stop, esc ends.
fn handle_zen_glance_key(
    key: KeyEvent,
    zen: &mut ZenState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) -> ZenKeyOutcome {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            zen.move_glance_selection(1);
            ZenKeyOutcome::Consumed
        }
        KeyCode::Char('k') | KeyCode::Up => {
            zen.move_glance_selection(-1);
            ZenKeyOutcome::Consumed
        }
        KeyCode::Enter => {
            if let Some(row) = zen.selected_glance().cloned()
                && let Some(part) = &row.part
            {
                // The entry may live in another change of the stack: load
                // its diff first so the jump lands on real rows. This is a
                // deliberate jump, so the home target is not restored.
                let desired = zen::row_target(&row, &zen.home_target);
                if session.target != desired {
                    match review_loader.load(session, desired.clone()) {
                        Ok(()) => reapply_agent_overlay(session, review_loader, tui_state),
                        Err(error) => {
                            tui_state.notice = Some(UiNotice {
                                level: UiNoticeLevel::Error,
                                message: format!("failed to load {desired}: {error:?}"),
                            });
                            return ZenKeyOutcome::End {
                                restore_target: false,
                            };
                        }
                    }
                }
                session.jump_to_chunk_part(part);
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: format!("zen ended — jumped to {}", part.path),
                });
            }
            ZenKeyOutcome::End {
                restore_target: false,
            }
        }
        KeyCode::Char('a') => {
            zen::mark_glance_viewed(session, zen);
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: format!(
                    "zen complete — {} glance item(s) marked viewed",
                    zen.glance_rows.len()
                ),
            });
            ZenKeyOutcome::End {
                restore_target: true,
            }
        }
        KeyCode::Char('p') | KeyCode::Left => {
            zen.phase = zen::ZenPhase::Focus;
            if let Some(stop) = zen.current().cloned() {
                zen_goto_stop(review_loader, session, zen, &stop, tui_state);
            }
            ZenKeyOutcome::Consumed
        }
        KeyCode::Esc => {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: "zen ended".to_owned(),
            });
            ZenKeyOutcome::End {
                restore_target: true,
            }
        }
        _ => {
            if keymap.action_for(&key) == Some(Action::Zen) {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "zen ended".to_owned(),
                });
                return ZenKeyOutcome::End {
                    restore_target: true,
                };
            }
            // The board is modal: swallow everything else so invisible
            // normal-mode actions cannot fire underneath it.
            ZenKeyOutcome::Consumed
        }
    }
}

/// Artifact viewer keys. A modal layer over the focus card: j/k (↑/↓)
/// scroll the exhibit, h/l (←/→, tab) cycle between exhibits, and
/// e/esc/enter/q close back to the card. Everything else is swallowed so
/// normal-mode actions cannot fire invisibly underneath.
fn handle_zen_artifact_key(
    key: KeyEvent,
    zen: &mut ZenState,
    index: usize,
    scroll: u16,
) -> ZenKeyOutcome {
    let count = zen
        .current()
        .map(|stop| zen::stop_artifacts(stop).len())
        .unwrap_or(0);
    if count == 0 {
        zen.phase = zen::ZenPhase::Focus;
        return ZenKeyOutcome::Consumed;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('e') | KeyCode::Char('q') => {
            zen.phase = zen::ZenPhase::Focus;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            zen.phase = zen::ZenPhase::Artifact {
                index,
                scroll: scroll.saturating_add(1),
            };
        }
        KeyCode::Char('k') | KeyCode::Up => {
            zen.phase = zen::ZenPhase::Artifact {
                index,
                scroll: scroll.saturating_sub(1),
            };
        }
        KeyCode::Char('l') | KeyCode::Right | KeyCode::Tab => {
            zen.phase = zen::ZenPhase::Artifact {
                index: (index + 1) % count,
                scroll: 0,
            };
        }
        KeyCode::Char('h') | KeyCode::Left => {
            zen.phase = zen::ZenPhase::Artifact {
                index: index.checked_sub(1).unwrap_or(count - 1),
                scroll: 0,
            };
        }
        _ => {}
    }
    ZenKeyOutcome::Consumed
}

/// Returns the next mode when the popup should change state.
fn handle_draft_list_key(
    key: KeyEvent,
    list: &mut DraftListState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    tui_state: &mut TuiState,
) -> Option<Mode> {
    if let Some(action) = keymap.target_picker_action_for(&key) {
        match action {
            Action::TargetPickerMoveDown => list.move_selection(1),
            Action::TargetPickerMoveUp => list.move_selection(-1),
            _ => {}
        }
        return None;
    }

    match key.code {
        KeyCode::Esc => Some(Mode::Normal),
        KeyCode::Enter | KeyCode::Char('a') => {
            let draft = list.selected_draft().cloned()?;
            if accept_agent_draft(session, tui_state, &draft.id, None) && list.remove(&draft.id) {
                return Some(Mode::Normal);
            }
            None
        }
        KeyCode::Char('e') => {
            let draft = list.selected_draft().cloned()?;
            Some(Mode::CommentInput {
                editor: CommentEditor {
                    cursor: draft.body.len(),
                    text: draft.body,
                },
                target: CommentInputTarget::AcceptDraft { id: draft.id },
            })
        }
        KeyCode::Char('x') => {
            let draft = list.selected_draft().cloned()?;
            session.discard_agent_draft(&draft.id);
            persist_draft_disposition(session, tui_state, &draft.id);
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: "discarded agent draft".to_owned(),
            });
            if list.remove(&draft.id) {
                return Some(Mode::Normal);
            }
            None
        }
        KeyCode::Char('j') | KeyCode::Down => {
            list.move_selection(1);
            None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            list.move_selection(-1);
            None
        }
        _ => None,
    }
}

/// Accept a pending agent draft (optionally with an edited body), persisting
/// the disposition to the overlay. Returns true on success.
fn accept_agent_draft(
    session: &mut ReviewSession,
    tui_state: &mut TuiState,
    draft_id: &str,
    body_override: Option<String>,
) -> bool {
    let Some(draft) = session
        .agent_drafts
        .iter()
        .find(|draft| draft.id == draft_id)
        .cloned()
    else {
        return false;
    };
    let body = body_override.unwrap_or_else(|| draft.body.clone());
    match session.accept_agent_draft(&draft, body) {
        Some(_comment_id) => {
            persist_draft_disposition(session, tui_state, draft_id);
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: "accepted agent draft as comment".to_owned(),
            });
            true
        }
        None => {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: format!("cannot accept draft: {} is not in this diff", draft.path),
            });
            false
        }
    }
}

/// Write a draft's disposition back into the shared overlay so agents can
/// observe the outcome. Refreshes the poll mtime so our own write does not
/// trigger a spurious reload notice.
fn persist_draft_disposition(session: &ReviewSession, tui_state: &mut TuiState, draft_id: &str) {
    let Some(overlay_path) = tui_state.agent_overlay_path.clone() else {
        return;
    };
    let Some(session_draft) = session
        .agent_drafts
        .iter()
        .find(|draft| draft.id == draft_id)
    else {
        return;
    };
    let result =
        crate::agent::AgentOverlay::load_or_default(&overlay_path).and_then(|mut overlay| {
            if let Some(draft) = overlay.drafts.iter_mut().find(|draft| draft.id == draft_id) {
                draft.state = session_draft.state;
                draft.accepted_comment_id = session_draft.accepted_comment_id.clone();
            }
            overlay.save(&overlay_path)?;
            Ok(())
        });
    match result {
        Ok(()) => {
            tui_state.overlay_mtime = std::fs::metadata(&overlay_path)
                .and_then(|metadata| metadata.modified())
                .ok();
        }
        Err(error) => {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: format!("failed to record draft disposition: {error}"),
            });
        }
    }
}

fn handle_file_search_key(
    key: KeyEvent,
    search: &mut FileSearchState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
) -> bool {
    if let Some(action) = keymap.target_picker_action_for(&key) {
        match action {
            Action::TargetPickerMoveDown => search.move_selection(1),
            Action::TargetPickerMoveUp => search.move_selection(-1),
            _ => {}
        }
        return false;
    }

    match key.code {
        KeyCode::Esc => true,
        KeyCode::Enter => {
            if let Some(file_index) = search.selected_file_index() {
                session.jump_to_file(file_index);
            }
            true
        }
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
) -> bool {
    if let Some(action) = keymap.target_picker_action_for(&key) {
        match action {
            Action::TargetPickerMoveDown => outline.move_selection(1),
            Action::TargetPickerMoveUp => outline.move_selection(-1),
            _ => {}
        }
        return false;
    }

    match key.code {
        KeyCode::Esc => true,
        KeyCode::Enter => {
            if let Some(row_index) = outline.selected_row_index() {
                session.jump_to_diff_row(row_index);
            }
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            outline.move_selection(1);
            false
        }
        KeyCode::Char('k') | KeyCode::Up => {
            outline.move_selection(-1);
            false
        }
        _ => false,
    }
}

fn handle_comment_list_key(
    key: KeyEvent,
    list: &mut CommentListState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    tui_state: &mut TuiState,
) -> bool {
    if let Some(action) = keymap.target_picker_action_for(&key) {
        match action {
            Action::TargetPickerMoveDown => list.move_selection(1, session),
            Action::TargetPickerMoveUp => list.move_selection(-1, session),
            _ => {}
        }
        return false;
    }

    match key.code {
        KeyCode::Esc => true,
        KeyCode::Enter => {
            if let Some(id) = list.selected_comment_id(session) {
                session.select_comment_by_id(&id);
            }
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            list.move_selection(1, session);
            false
        }
        KeyCode::Char('k') | KeyCode::Up => {
            list.move_selection(-1, session);
            false
        }
        KeyCode::Char('s') => {
            if let Some(id) = list.selected_comment_id(session)
                && let Some(state) = session.cycle_comment_state(&id)
            {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: format!("comment marked {}", state.label()),
                });
            }
            false
        }
        KeyCode::Char('a') => {
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
            false
        }
        KeyCode::Char('K') => {
            if let Some(id) = list.selected_comment_id(session)
                && let Some(kind) = session.cycle_comment_kind(&id)
            {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: format!("comment kind {}", kind.map_or("none", comment_kind_label)),
                });
            }
            false
        }
        KeyCode::Char('x') => {
            if let Some(id) = list.selected_comment_id(session) {
                session.delete_comment(&id);
                tui_state.state_tombstones.comments.insert(id);
                list.clamp(session);
            }
            session.comments.is_empty()
        }
        _ => false,
    }
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

fn handle_task_list_key(
    key: KeyEvent,
    list: &mut TaskListState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    tui_state: &mut TuiState,
) -> bool {
    if let Some(action) = keymap.target_picker_action_for(&key) {
        match action {
            Action::TargetPickerMoveDown => list.move_selection(1),
            Action::TargetPickerMoveUp => list.move_selection(-1),
            _ => {}
        }
        return false;
    }

    match key.code {
        KeyCode::Esc => true,
        KeyCode::Enter => {
            if let Some(id) = list.selected_comment_id() {
                session.select_comment_by_id(id);
            }
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            list.move_selection(1);
            false
        }
        KeyCode::Char('k') | KeyCode::Up => {
            list.move_selection(-1);
            false
        }
        KeyCode::Char('d') => {
            if let Some(id) = list.selected_comment_id().map(str::to_owned)
                && let Some(state) = session.cycle_comment_state(&id)
            {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: format!("comment marked {}", state.label()),
                });
                list.refresh(session);
            }
            list.comment_ids.is_empty()
        }
        _ => false,
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
    if let Some(action) = keymap.target_picker_action_for(&key) {
        match action {
            Action::TargetPickerMoveDown => list.move_selection(1, len),
            Action::TargetPickerMoveUp => list.move_selection(-1, len),
            _ => {}
        }
        return false;
    }
    match key.code {
        KeyCode::Esc => true,
        KeyCode::Char('j') | KeyCode::Char('n') | KeyCode::Down | KeyCode::Right => {
            list.move_selection(1, len);
            false
        }
        KeyCode::Char('k') | KeyCode::Char('e') | KeyCode::Up | KeyCode::Left => {
            list.move_selection(-1, len);
            false
        }
        KeyCode::Enter => {
            if let Some(event) = tui_state.activity.iter().rev().nth(list.selected)
                && let Some(index) = session
                    .files
                    .iter()
                    .position(|file| event.message.starts_with(&file.path))
            {
                session.select_file_index(index);
                return true;
            }
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: "no file target for this event".to_owned(),
            });
            false
        }
        _ => false,
    }
}

fn handle_walkthrough_list_key(
    key: KeyEvent,
    list: &mut WalkthroughListState,
    session: &mut ReviewSession,
    keymap: &KeyMap,
    tui_state: &mut TuiState,
) -> bool {
    if let Some(action) = keymap.target_picker_action_for(&key) {
        match action {
            Action::TargetPickerMoveDown => list.move_selection(1),
            Action::TargetPickerMoveUp => list.move_selection(-1),
            _ => {}
        }
        return false;
    }
    match key.code {
        KeyCode::Esc => true,
        KeyCode::Enter => {
            if let Some(step) = selected_walkthrough_step(session, list).cloned() {
                jump_to_walkthrough_step(session, &step);
            }
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            list.move_selection(1);
            false
        }
        KeyCode::Char('k') | KeyCode::Up => {
            list.move_selection(-1);
            false
        }
        KeyCode::Char('d') => {
            if let Some(id) = list.selected_step_id().map(str::to_owned) {
                if let Some(durable) = session.sessions.iter_mut().find(|s| {
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
            list.step_ids.is_empty()
        }
        KeyCode::Char('J') => {
            move_selected_walkthrough_step(session, list, 1);
            false
        }
        KeyCode::Char('K') => {
            move_selected_walkthrough_step(session, list, -1);
            false
        }
        _ => false,
    }
}

fn selected_walkthrough_step<'a>(
    session: &'a ReviewSession,
    list: &WalkthroughListState,
) -> Option<&'a WalkthroughStep> {
    let id = list.selected_step_id()?;
    session
        .sessions
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
    for durable in &mut session.sessions {
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

fn jump_to_walkthrough_step(session: &mut ReviewSession, step: &WalkthroughStep) {
    let Some(path) = &step.target.file else {
        return;
    };
    let Some(file_index) = session.files.iter().position(|file| &file.path == path) else {
        return;
    };
    session.select_file_index(file_index);
    session.focus = Focus::Diff;
    if let Some(line) = step.target.line
        && let Some(row_index) = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.new_lineno == Some(line) || row.old_lineno == Some(line))
    {
        session.jump_to_diff_row(row_index);
    }
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
        session.annotate_generated_where(|file| {
            self.generated_matcher.is_match(&file.path)
                || crate::generated::diff_content_looks_generated(&file.diff)
        });
        Ok(())
    }
}

fn load_review_target(
    review_loader: &ReviewLoader<'_>,
    session: &mut ReviewSession,
    target: ReviewTarget,
    tui_state: &mut TuiState,
) {
    match review_loader.load(session, target.clone()) {
        Ok(()) => {
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
            let body = std::mem::take(editor).into_text();
            match target {
                CommentInputTarget::New => session.add_comment(body),
                CommentInputTarget::Edit { id } => {
                    session.update_comment_body(id, body);
                }
                CommentInputTarget::AcceptDraft { id } => {
                    accept_agent_draft(session, tui_state, id, Some(body));
                }
            }
            return true;
        }
        Action::InsertNewline => editor.insert_newline(),
        Action::DeleteChar => {
            editor.backspace();
        }
        _ => {}
    }
    false
}

fn handle_comment_key(key: KeyEvent, editor: &mut CommentEditor) {
    match (key.code, key.modifiers) {
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => editor.delete_to_line_start(),
        (KeyCode::Char('k'), KeyModifiers::CONTROL) => editor.delete_to_line_end(),
        (KeyCode::Char('w'), KeyModifiers::CONTROL) => editor.delete_previous_word(),
        (KeyCode::Char('a'), KeyModifiers::CONTROL) => editor.move_to_line_start(),
        (KeyCode::Char('e'), KeyModifiers::CONTROL) => editor.move_to_line_end(),
        (KeyCode::Char('b'), KeyModifiers::ALT) | (KeyCode::Left, KeyModifiers::CONTROL) => {
            editor.move_word_left();
        }
        (KeyCode::Char('f'), KeyModifiers::ALT) | (KeyCode::Right, KeyModifiers::CONTROL) => {
            editor.move_word_right();
        }
        (KeyCode::Char(ch), modifiers) if modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
            editor.insert_char(ch);
        }
        (KeyCode::Left, _) => editor.move_left(),
        (KeyCode::Right, _) => editor.move_right(),
        (KeyCode::Up, _) => editor.move_up(),
        (KeyCode::Down, _) => editor.move_down(),
        _ => {}
    }
}

fn handle_mouse_event(
    mouse: MouseEvent,
    terminal_size: ratatui::prelude::Size,
    session: &mut ReviewSession,
    mode: &mut Mode,
    tui_state: &mut TuiState,
) {
    if handle_zen_mouse_event(mouse, terminal_size, session, tui_state) {
        return;
    }

    if matches!(
        mode,
        Mode::Help
            | Mode::CommentInput { .. }
            | Mode::RevsetInput(_)
            | Mode::OperationPicker(_)
            | Mode::JjHelpers(_)
            | Mode::FlagList(_)
            | Mode::TaskList(_)
            | Mode::Activity(_)
            | Mode::WalkthroughList(_)
            | Mode::DraftList(_)
            | Mode::FileSearch(_)
            | Mode::SymbolOutline(_)
            | Mode::CommentList(_)
    ) {
        return;
    }

    let layout = ui_layout(
        Rect::new(0, 0, terminal_size.width, terminal_size.height),
        session.file_pane_visible,
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
            session.scroll_diff(3);
        }
        MouseEventKind::ScrollUp if point_in_rect(mouse.column, mouse.row, layout.diff) => {
            session.scroll_diff(-3);
        }
        _ => {}
    }
}

fn handle_zen_mouse_event(
    mouse: MouseEvent,
    terminal_size: ratatui::prelude::Size,
    session: &mut ReviewSession,
    tui_state: &mut TuiState,
) -> bool {
    let Some(zen) = tui_state.zen.as_mut() else {
        return false;
    };
    let layout = ui_layout(
        Rect::new(0, 0, terminal_size.width, terminal_size.height),
        session.file_pane_visible,
    );
    let body = Rect {
        height: terminal_size.height.saturating_sub(2),
        ..Rect::new(0, 0, terminal_size.width, terminal_size.height)
    };

    match mouse.kind {
        MouseEventKind::ScrollDown => match zen.phase {
            zen::ZenPhase::Artifact { index, scroll }
                if point_in_rect(mouse.column, mouse.row, body) =>
            {
                zen.phase = zen::ZenPhase::Artifact {
                    index,
                    scroll: scroll.saturating_add(3),
                };
                true
            }
            zen::ZenPhase::Glance if point_in_rect(mouse.column, mouse.row, body) => {
                zen.move_glance_selection(1);
                true
            }
            zen::ZenPhase::Focus if point_in_rect(mouse.column, mouse.row, body) => {
                session.focus = Focus::Diff;
                session.move_diff_cursor(3);
                true
            }
            zen::ZenPhase::Reading if point_in_rect(mouse.column, mouse.row, layout.diff) => {
                session.scroll_diff(3);
                true
            }
            _ => false,
        },
        MouseEventKind::ScrollUp => match zen.phase {
            zen::ZenPhase::Artifact { index, scroll }
                if point_in_rect(mouse.column, mouse.row, body) =>
            {
                zen.phase = zen::ZenPhase::Artifact {
                    index,
                    scroll: scroll.saturating_sub(3),
                };
                true
            }
            zen::ZenPhase::Glance if point_in_rect(mouse.column, mouse.row, body) => {
                zen.move_glance_selection(-1);
                true
            }
            zen::ZenPhase::Focus if point_in_rect(mouse.column, mouse.row, body) => {
                session.focus = Focus::Diff;
                session.move_diff_cursor(-3);
                true
            }
            zen::ZenPhase::Reading if point_in_rect(mouse.column, mouse.row, layout.diff) => {
                session.scroll_diff(-3);
                true
            }
            _ => false,
        },
        _ => false,
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
            session.select_visible_tree_row(row);
        }
        tui_state.diff_drag = None;
        return;
    }

    let diff_inner = inner_bordered(layout.diff);
    if point_in_rect(x, y, diff_inner)
        && let Some(visible_row) = row_in_inner(y, diff_inner)
    {
        let row_index = session.diff_scroll as usize + visible_row;
        session.clear_diff_range_selection();
        session.select_diff_row(row_index);
        tui_state.diff_drag = Some(DiffDrag {
            start_row: row_index,
            current_row: row_index,
            saw_drag: false,
        });
    }
}

fn handle_left_drag(
    x: u16,
    y: u16,
    layout: render::UiLayout,
    session: &mut ReviewSession,
    tui_state: &mut TuiState,
) {
    let Some(drag) = tui_state.diff_drag.as_mut() else {
        return;
    };
    let diff_inner = inner_bordered(layout.diff);
    if point_in_rect(x, y, diff_inner)
        && let Some(visible_row) = row_in_inner(y, diff_inner)
    {
        drag.current_row = session.diff_scroll as usize + visible_row;
        drag.saw_drag = true;
        session.set_diff_range_selection(drag.start_row, drag.current_row);
    }
}

fn handle_left_up(session: &mut ReviewSession, mode: &mut Mode, tui_state: &mut TuiState) {
    let Some(drag) = tui_state.diff_drag.take() else {
        return;
    };
    if drag.saw_drag && session.selected_range_anchor().is_some() {
        *mode = Mode::CommentInput {
            editor: CommentEditor::default(),
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
    use std::{cell::RefCell, path::Path};

    use crate::jj::{JjBackend, JjChangeSummary, ReviewTarget};
    use crate::state::{ActionIntent, Comment, CommentKind, CommentState, ReviewTask};

    struct MockJjBackend {
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
            path: "src/lib.rs".to_owned(),
            line: Some(1),
            end_line: None,
            anchor: None,
            body: "fix this".to_owned(),
            kind: Some(CommentKind::Issue),
            action: Some(ActionIntent::Fix),
            state: CommentState::Todo,
            created_at: chrono::Utc::now(),
            ..Default::default()
        });
        ensure_tui_review_session(&mut session)
            .tasks
            .push(ReviewTask {
                id: "t1".to_owned(),
                title: "do the thing".to_owned(),
                action: ActionIntent::Fix,
                ..ReviewTask::default()
            });
        let copied = RefCell::new(String::new());
        let mut tui_state = TuiState::default();
        yank_handoff_with(&session, &mut tui_state, |body| {
            copied.replace(body.to_owned());
            Ok(ClipboardMethod::Osc52)
        });

        assert!(copied.borrow().starts_with("# Human review handoff"));
        assert!(copied.borrow().contains("fix this"));
        assert_eq!(
            tui_state.notice,
            Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: "handoff copied via OSC52 (/dev/tty) (2 action items)".to_owned(),
            })
        );
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

    #[test]
    fn reapply_agent_overlay_preserves_change_anchored_chunks_with_loaded_change_diff() {
        let dir = tempfile::tempdir().unwrap();
        let overlay_path = dir.path().join("agent.json");
        crate::agent::AgentOverlay {
            chunks: vec![crate::agent::ReviewChunk {
                id: "c1".to_owned(),
                title: "anchored".to_owned(),
                importance: crate::agent::ChunkImportance::Spotlight,
                change_id: Some("change1".to_owned()),
                rationale: None,
                explanation: None,
                artifacts: Vec::new(),
                parts: vec![crate::agent::ChunkPart {
                    path: "src/lib.rs".to_owned(),
                    start_line: Some(1),
                    end_line: Some(1),
                }],
            }],
            ..Default::default()
        }
        .save(&overlay_path)
        .unwrap();
        let mut session =
            snapshot_session("diff --git a/src/lib.rs b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n");
        let backend = MockJjBackend::with_diff(Ok(
            "diff --git a/src/lib.rs b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n".to_owned(),
        ));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState {
            agent_overlay_path: Some(overlay_path),
            ..Default::default()
        };

        reapply_agent_overlay(&mut session, &loader, &mut tui_state);

        assert_eq!(session.review_chunks.len(), 1);
        assert_eq!(session.review_chunks[0].parts.len(), 1);
        assert_eq!(backend.calls.borrow().len(), 1);
        assert_eq!(backend.calls.borrow()[0].rev, "change1");
        assert!(tui_state.notice.is_none());
    }

    #[test]
    fn reapply_agent_overlay_warns_when_change_anchored_chunks_really_invalidate() {
        let dir = tempfile::tempdir().unwrap();
        let overlay_path = dir.path().join("agent.json");
        crate::agent::AgentOverlay {
            chunks: vec![crate::agent::ReviewChunk {
                id: "c1".to_owned(),
                title: "anchored".to_owned(),
                importance: crate::agent::ChunkImportance::Spotlight,
                change_id: Some("change1".to_owned()),
                rationale: None,
                explanation: None,
                artifacts: Vec::new(),
                parts: vec![crate::agent::ChunkPart {
                    path: "src/missing.rs".to_owned(),
                    start_line: Some(1),
                    end_line: Some(1),
                }],
            }],
            ..Default::default()
        }
        .save(&overlay_path)
        .unwrap();
        let mut session =
            snapshot_session("diff --git a/src/lib.rs b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n");
        let backend = MockJjBackend::with_diff(Ok(
            "diff --git a/src/lib.rs b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n".to_owned(),
        ));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState {
            agent_overlay_path: Some(overlay_path),
            ..Default::default()
        };

        reapply_agent_overlay(&mut session, &loader, &mut tui_state);

        assert!(session.review_chunks.is_empty());
        assert_eq!(tui_state.invalid_chunk_parts.len(), 1);
        assert_eq!(
            tui_state
                .notice
                .as_ref()
                .map(|notice| notice.message.as_str()),
            Some(
                "1 curated walkthrough part(s) no longer match the diff; update the agent overlay or durable walkthrough targets"
            )
        );
        assert!(
            tui_state
                .activity
                .back()
                .is_some_and(|event| event.message.contains("curated walkthrough part"))
        );
    }

    impl JjBackend for MockJjBackend {
        fn snapshot_working_copy(&self, _repo: &Path) -> Result<()> {
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
            last_autosave: Some(state_fingerprint(&session)),
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

        // Unchanged session: fingerprint short-circuits the write.
        let modified_before = std::fs::metadata(&state_path).unwrap().modified().unwrap();
        autosave_state(&mut session, &state_path, &mut tui_state);
        let modified_after = std::fs::metadata(&state_path).unwrap().modified().unwrap();
        assert_eq!(modified_before, modified_after);
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
            path: "b.txt".to_owned(),
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
            last_autosave: Some(state_fingerprint(&session)),
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
        assert!(saved.comments.iter().any(|comment| comment.path == "a.txt"));
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
    fn comment_editor_can_update_existing_comment() {
        let mut session = snapshot_session(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        );
        session.add_comment("old body".into());
        let id = session.comments[0].id.clone();
        let mut editor = CommentEditor {
            text: "new body".to_owned(),
            cursor: "new body".len(),
        };
        let target = CommentInputTarget::Edit { id };

        assert!(handle_comment_action(
            Action::SubmitComment,
            &mut session,
            &mut editor,
            &target,
            &mut TuiState::default(),
        ));

        assert_eq!(session.comments.len(), 1);
        assert_eq!(session.comments[0].body, "new body");
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
        let mut input = RevsetInputState::new("", "");
        for ch in "ancestors(@, 2)".chars() {
            assert!(!handle_revset_input_key(
                KeyEvent::from(KeyCode::Char(ch)),
                &mut input,
                &mut session,
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
                &loader,
                &mut tui_state,
            );
        }

        assert!(handle_revset_input_key(
            KeyEvent::from(KeyCode::Enter),
            &mut input,
            &mut session,
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
        let mut input = RevsetInputState::new("", "@");

        assert!(!handle_revset_input_key(
            KeyEvent::from(KeyCode::Enter),
            &mut input,
            &mut session,
            &loader,
            &mut tui_state,
        ));

        assert!(backend.calls.borrow().is_empty());
        let notice = tui_state.notice.unwrap();
        assert_eq!(notice.level, UiNoticeLevel::Info);
        assert!(notice.message.contains("required"));
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
            path: "a.txt".to_owned(),
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
            last_autosave: Some(state_fingerprint(&session)),
            ..TuiState::default()
        };

        let mut external = crate::state::ReviewState::load_or_default(&state_path).unwrap();
        external.comments.push(Comment {
            id: "external".to_owned(),
            path: "a.txt".to_owned(),
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
            move_down: vec!["n".to_owned()],
            move_up: vec!["e".to_owned()],
            target_picker_down: vec!["ctrl-j".to_owned()],
            target_picker_up: vec!["ctrl-k".to_owned()],
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
            KeyEvent::from(KeyCode::Char('n')),
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
            KeyEvent::from(KeyCode::Char('e')),
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
            "agent suggestions updated"
        );

        // Unchanged mtime: no re-notification.
        tui_state.notice = None;
        maybe_reload_agent_overlay(&mut session, &overlay_path, &mut tui_state, &loader, true);
        assert!(tui_state.notice.is_none());
    }

    #[test]
    fn overlay_polling_ignores_invalid_chunk_parts_with_notice() {
        let dir = tempfile::tempdir().unwrap();
        let overlay_path = dir.path().join("agent.json");
        let mut session = snapshot_session(
            r#"diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1 +1 @@
-old
+new
"#,
        );
        crate::agent::AgentOverlay {
            chunks: vec![crate::agent::ReviewChunk {
                id: "c1".to_owned(),
                title: "mixed".to_owned(),
                importance: crate::agent::ChunkImportance::Spotlight,
                change_id: None,
                rationale: None,
                explanation: None,
                artifacts: Vec::new(),
                parts: vec![
                    crate::agent::ChunkPart {
                        path: "a.rs".to_owned(),
                        start_line: Some(1),
                        end_line: Some(1),
                    },
                    crate::agent::ChunkPart {
                        path: "missing.rs".to_owned(),
                        start_line: Some(1),
                        end_line: Some(1),
                    },
                ],
            }],
            ..Default::default()
        }
        .save(&overlay_path)
        .unwrap();
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();

        maybe_reload_agent_overlay(&mut session, &overlay_path, &mut tui_state, &loader, true);

        assert_eq!(session.review_chunks.len(), 1);
        assert_eq!(session.review_chunks[0].parts.len(), 1);
        assert_eq!(session.review_chunks[0].parts[0].path, "a.rs");
        assert!(
            tui_state
                .notice
                .unwrap()
                .message
                .contains("1 invalid chunk part")
        );
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
        let overlay = crate::agent::AgentOverlay {
            drafts: vec![crate::agent::AgentDraft {
                id: "draft-1".to_owned(),
                path: "a.txt".to_owned(),
                line: Some(1),
                body: "agent thinks this is wrong".to_owned(),
                state: crate::agent::DraftState::Pending,
                accepted_comment_id: None,
            }],
            ..Default::default()
        };
        overlay.save(&overlay_path).unwrap();
        session.apply_agent_overlay(&overlay);
        (session, overlay_path)
    }

    #[test]
    fn accepting_a_draft_creates_comment_and_persists_disposition() {
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

        let on_disk = crate::agent::AgentOverlay::load_or_default(&overlay_path).unwrap();
        assert_eq!(on_disk.drafts[0].state, crate::agent::DraftState::Accepted);
        assert_eq!(
            on_disk.drafts[0].accepted_comment_id.as_deref(),
            Some(session.comments[0].id.as_str())
        );
    }

    #[test]
    fn discarding_a_draft_persists_without_creating_comment() {
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
        let on_disk = crate::agent::AgentOverlay::load_or_default(&overlay_path).unwrap();
        assert_eq!(on_disk.drafts[0].state, crate::agent::DraftState::Discarded);
        assert_eq!(on_disk.drafts[0].accepted_comment_id, None);
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
        let on_disk = crate::agent::AgentOverlay::load_or_default(&overlay_path).unwrap();
        assert_eq!(on_disk.drafts[0].state, crate::agent::DraftState::Accepted);
    }

    #[test]
    fn accepting_draft_for_missing_file_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let (mut session, overlay_path) = draft_session_with_overlay(dir.path());
        session.agent_drafts[0].path = "gone.rs".to_owned();
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

        assert!(session.comments.is_empty());
        let notice = tui_state.notice.unwrap();
        assert_eq!(notice.level, UiNoticeLevel::Error);
        assert!(notice.message.contains("gone.rs"));
        let on_disk = crate::agent::AgentOverlay::load_or_default(&overlay_path).unwrap();
        assert_eq!(on_disk.drafts[0].state, crate::agent::DraftState::Pending);
    }

    fn zen_session() -> ReviewSession {
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
        session.apply_agent_overlay(&crate::agent::AgentOverlay {
            chunks: vec![crate::agent::ReviewChunk {
                id: "c1".to_owned(),
                title: "core flow".to_owned(),
                importance: crate::agent::ChunkImportance::Spotlight,
                change_id: None,
                explanation: None,
                rationale: Some("read together".to_owned()),
                artifacts: Vec::new(),
                parts: vec![
                    crate::agent::ChunkPart {
                        path: "a.rs".to_owned(),
                        start_line: Some(1),
                        end_line: Some(1),
                    },
                    crate::agent::ChunkPart {
                        path: "b.rs".to_owned(),
                        start_line: Some(1),
                        end_line: Some(1),
                    },
                ],
            }],
            ..Default::default()
        });
        session
    }

    /// Loader over a mock backend for zen navigation tests (retargeting
    /// change-anchored stops goes through the loader).
    fn zen_loader(backend: &MockJjBackend) -> ReviewLoader<'_> {
        ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: backend,
        }
    }

    #[test]
    fn zen_advances_through_stops_marking_files_viewed() {
        let mut session = zen_session();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let zen_backend = MockJjBackend::with_diff(Ok(String::new()));
        let mut tui_state = TuiState::default();
        let mut zen = ZenState::new(&session, &[]).unwrap();

        // Advancing past the opening chapter card marks nothing viewed and
        // lands on the first stop.
        assert!(matches!(
            handle_zen_key(
                KeyEvent::from(KeyCode::Enter),
                &mut zen,
                &mut session,
                &keymap,
                &zen_loader(&zen_backend),
                &mut tui_state,
            ),
            ZenKeyOutcome::Consumed
        ));
        assert!(session.files.iter().all(|file| !file.viewed));
        assert_eq!(session.zen_focus.as_ref().unwrap().path, "a.rs");

        // Advancing past the first stop marks a.rs viewed and moves on.
        assert!(matches!(
            handle_zen_key(
                KeyEvent::from(KeyCode::Enter),
                &mut zen,
                &mut session,
                &keymap,
                &zen_loader(&zen_backend),
                &mut tui_state,
            ),
            ZenKeyOutcome::Consumed
        ));
        assert!(
            session
                .files
                .iter()
                .find(|file| file.path == "a.rs")
                .unwrap()
                .viewed
        );
        assert_eq!(session.selected_file().unwrap().path, "b.rs");
        assert_eq!(session.zen_focus.as_ref().unwrap().path, "b.rs");

        // Advancing past the last stop ends the walkthrough with a notice.
        assert!(matches!(
            handle_zen_key(
                KeyEvent::from(KeyCode::Enter),
                &mut zen,
                &mut session,
                &keymap,
                &zen_loader(&zen_backend),
                &mut tui_state,
            ),
            ZenKeyOutcome::End { .. }
        ));
        assert!(session.files.iter().all(|file| file.viewed));
        assert!(tui_state.notice.unwrap().message.contains("zen complete"));
    }

    #[test]
    fn zen_esc_ends_the_walkthrough_without_marking_viewed() {
        let mut session = zen_session();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let zen_backend = MockJjBackend::with_diff(Ok(String::new()));
        let mut tui_state = TuiState::default();
        let mut zen = ZenState::new(&session, &[]).unwrap();

        assert!(matches!(
            handle_zen_key(
                KeyEvent::from(KeyCode::Esc),
                &mut zen,
                &mut session,
                &keymap,
                &zen_loader(&zen_backend),
                &mut tui_state,
            ),
            ZenKeyOutcome::End { .. }
        ));
        assert!(session.files.iter().all(|file| !file.viewed));
    }

    #[test]
    fn zen_esc_cancels_an_active_range_selection_first() {
        let mut session = zen_session();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let zen_backend = MockJjBackend::with_diff(Ok(String::new()));
        let mut tui_state = TuiState::default();
        let mut zen = ZenState::new(&session, &[]).unwrap();
        zen.advance(); // past the chapter card onto the first stop
        zen::jump_to_stop(&mut session, &zen.stops[1].clone());
        session.toggle_diff_range_selection();
        assert!(session.has_active_diff_range());

        // Esc falls through to the normal vocabulary, which cancels the
        // range; the walkthrough survives.
        assert!(matches!(
            handle_zen_key(
                KeyEvent::from(KeyCode::Esc),
                &mut zen,
                &mut session,
                &keymap,
                &zen_loader(&zen_backend),
                &mut tui_state,
            ),
            ZenKeyOutcome::Fallthrough
        ));
    }

    #[test]
    fn zen_lets_review_keys_fall_through() {
        let mut session = zen_session();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let zen_backend = MockJjBackend::with_diff(Ok(String::new()));
        let mut tui_state = TuiState::default();
        let mut zen = ZenState::new(&session, &[]).unwrap();

        // A comment key is not a zen navigation key: the caller routes it
        // to the normal-mode vocabulary.
        assert!(matches!(
            handle_zen_key(
                KeyEvent::from(KeyCode::Char('c')),
                &mut zen,
                &mut session,
                &keymap,
                &zen_loader(&zen_backend),
                &mut tui_state,
            ),
            ZenKeyOutcome::Fallthrough
        ));
    }

    #[test]
    fn zen_key_ends_the_walkthrough() {
        let mut session = zen_session();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let zen_backend = MockJjBackend::with_diff(Ok(String::new()));
        let mut tui_state = TuiState::default();
        let mut zen = ZenState::new(&session, &[]).unwrap();

        assert!(matches!(
            handle_zen_key(
                KeyEvent::from(KeyCode::Char('Z')),
                &mut zen,
                &mut session,
                &keymap,
                &zen_loader(&zen_backend),
                &mut tui_state,
            ),
            ZenKeyOutcome::End { .. }
        ));
        assert!(tui_state.notice.unwrap().message.contains("zen ended"));
    }

    /// A session where the spotlight covers only a.rs, leaving b.rs for the
    /// glance board.
    fn zen_session_with_glance() -> ReviewSession {
        let mut session = zen_session();
        session.apply_agent_overlay(&crate::agent::AgentOverlay {
            chunks: vec![crate::agent::ReviewChunk {
                id: "c1".to_owned(),
                title: "the important bit".to_owned(),
                importance: crate::agent::ChunkImportance::Spotlight,
                change_id: None,
                rationale: None,
                explanation: Some("This is the heart of the change.".to_owned()),
                artifacts: Vec::new(),
                parts: vec![crate::agent::ChunkPart {
                    path: "a.rs".to_owned(),
                    start_line: Some(1),
                    end_line: Some(1),
                }],
            }],
            ..Default::default()
        });
        session
    }

    #[test]
    fn zen_opens_the_glance_board_after_the_last_stop() {
        let mut session = zen_session_with_glance();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let zen_backend = MockJjBackend::with_diff(Ok(String::new()));
        let mut tui_state = TuiState::default();
        let mut zen = ZenState::new(&session, &[]).unwrap();
        assert_eq!(zen.stops.len(), 2); // chapter card + one spotlight
        assert!(zen.has_glance());

        // Step off the chapter card onto the only stop.
        assert!(matches!(
            handle_zen_key(
                KeyEvent::from(KeyCode::Enter),
                &mut zen,
                &mut session,
                &keymap,
                &zen_loader(&zen_backend),
                &mut tui_state,
            ),
            ZenKeyOutcome::Consumed
        ));
        assert_eq!(zen.phase, zen::ZenPhase::Focus);

        // Advancing past the only stop lands on the glance board instead of
        // ending, so the boilerplate is skimmed rather than skipped.
        assert!(matches!(
            handle_zen_key(
                KeyEvent::from(KeyCode::Enter),
                &mut zen,
                &mut session,
                &keymap,
                &zen_loader(&zen_backend),
                &mut tui_state,
            ),
            ZenKeyOutcome::Consumed
        ));
        assert_eq!(zen.phase, zen::ZenPhase::Glance);

        // `a` bulk-acknowledges the glance items and finishes the briefing.
        assert!(matches!(
            handle_zen_key(
                KeyEvent::from(KeyCode::Char('a')),
                &mut zen,
                &mut session,
                &keymap,
                &zen_loader(&zen_backend),
                &mut tui_state,
            ),
            ZenKeyOutcome::End { .. }
        ));
        assert!(session.files.iter().all(|file| file.viewed));
        assert!(tui_state.notice.unwrap().message.contains("zen complete"));
    }

    #[test]
    fn zen_dot_refocuses_the_current_stop_after_wandering() {
        let mut session = zen_session_with_glance();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let zen_backend = MockJjBackend::with_diff(Ok(String::new()));
        let mut tui_state = TuiState::default();
        let mut zen = ZenState::new(&session, &[]).unwrap();
        zen.advance(); // past the chapter card onto the stop
        zen::jump_to_stop(&mut session, &zen.stops[1].clone());
        let home = session.diff_cursor;

        // Wander off the stop with normal line navigation.
        session.move_diff_cursor(-1);
        assert_ne!(session.diff_cursor, home);

        assert!(matches!(
            handle_zen_key(
                KeyEvent::from(KeyCode::Char('.')),
                &mut zen,
                &mut session,
                &keymap,
                &zen_loader(&zen_backend),
                &mut tui_state,
            ),
            ZenKeyOutcome::Consumed
        ));
        assert_eq!(session.diff_cursor, home);
        assert_eq!(session.zen_focus.as_ref().unwrap().path, "a.rs");
    }

    #[test]
    fn zen_tab_toggles_between_focus_card_and_reading_view() {
        let mut session = zen_session_with_glance();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let zen_backend = MockJjBackend::with_diff(Ok(String::new()));
        let mut tui_state = TuiState::default();
        let mut zen = ZenState::new(&session, &[]).unwrap();
        assert_eq!(zen.phase, zen::ZenPhase::Focus);

        for expected in [zen::ZenPhase::Reading, zen::ZenPhase::Focus] {
            assert!(matches!(
                handle_zen_key(
                    KeyEvent::from(KeyCode::Tab),
                    &mut zen,
                    &mut session,
                    &keymap,
                    &zen_loader(&zen_backend),
                    &mut tui_state,
                ),
                ZenKeyOutcome::Consumed
            ));
            assert_eq!(zen.phase, expected);
        }
    }

    #[test]
    fn zen_d_collapses_the_chapter_description_but_only_on_chapter_cards() {
        let mut session = zen_session_with_glance();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let zen_backend = MockJjBackend::with_diff(Ok(String::new()));
        let mut tui_state = TuiState::default();
        let mut zen = ZenState::new(&session, &[]).unwrap();
        assert!(!zen.chapter_description_collapsed);

        // On the chapter card `d` toggles the description body.
        assert!(matches!(
            handle_zen_key(
                KeyEvent::from(KeyCode::Char('d')),
                &mut zen,
                &mut session,
                &keymap,
                &zen_loader(&zen_backend),
                &mut tui_state,
            ),
            ZenKeyOutcome::Consumed
        ));
        assert!(zen.chapter_description_collapsed);

        // On a spotlight stop `d` is not a zen key: the normal vocabulary
        // keeps it.
        zen.advance();
        assert!(matches!(
            handle_zen_key(
                KeyEvent::from(KeyCode::Char('d')),
                &mut zen,
                &mut session,
                &keymap,
                &zen_loader(&zen_backend),
                &mut tui_state,
            ),
            ZenKeyOutcome::Fallthrough
        ));
        assert!(zen.chapter_description_collapsed);
    }

    #[test]
    fn zen_e_opens_scrolls_and_closes_the_artifact_viewer() {
        let mut session = zen_session_with_glance();
        // Attach two exhibits to the spotlight chunk.
        session.review_chunks[0].artifacts = vec![
            crate::agent::Artifact {
                title: "usage".to_owned(),
                kind: crate::agent::ArtifactKind::Example,
                body: "line one\nline two\nline three".to_owned(),
            },
            crate::agent::Artifact {
                title: "test run".to_owned(),
                kind: crate::agent::ArtifactKind::Output,
                body: "3 passed".to_owned(),
            },
        ];
        let spotlight_id = session.review_chunks[0].id.clone();
        let artifacts: Vec<_> = session.review_chunks[0]
            .artifacts
            .iter()
            .map(|artifact| crate::state::StepArtifact {
                title: artifact.title.clone(),
                kind: match artifact.kind {
                    crate::agent::ArtifactKind::Example => crate::state::StepArtifactKind::Example,
                    crate::agent::ArtifactKind::Output => crate::state::StepArtifactKind::Output,
                    crate::agent::ArtifactKind::Diagram => crate::state::StepArtifactKind::Diagram,
                    crate::agent::ArtifactKind::Note => crate::state::StepArtifactKind::Note,
                },
                body: artifact.body.clone(),
            })
            .collect();
        for step in session
            .sessions
            .iter_mut()
            .flat_map(|durable| durable.walkthroughs.iter_mut())
            .flat_map(|walkthrough| walkthrough.steps.iter_mut())
        {
            if step.id == spotlight_id {
                step.artifacts = artifacts.clone();
            }
        }
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let zen_backend = MockJjBackend::with_diff(Ok(String::new()));
        let mut tui_state = TuiState::default();
        let mut zen = ZenState::new(&session, &[]).unwrap();
        zen.advance(); // chapter card -> the spotlight stop

        // `e` on the chapter card would find no artifacts; on the stop it
        // opens the viewer.
        let mut press = |code: KeyCode, zen: &mut ZenState, session: &mut ReviewSession| {
            assert!(matches!(
                handle_zen_key(
                    KeyEvent::from(code),
                    zen,
                    session,
                    &keymap,
                    &zen_loader(&zen_backend),
                    &mut tui_state,
                ),
                ZenKeyOutcome::Consumed
            ));
        };
        press(KeyCode::Char('e'), &mut zen, &mut session);
        assert_eq!(
            zen.phase,
            zen::ZenPhase::Artifact {
                index: 0,
                scroll: 0
            }
        );

        // j scrolls, l cycles to the next exhibit (resetting scroll), h
        // wraps back, esc closes.
        press(KeyCode::Char('j'), &mut zen, &mut session);
        assert_eq!(
            zen.phase,
            zen::ZenPhase::Artifact {
                index: 0,
                scroll: 1
            }
        );
        press(KeyCode::Char('l'), &mut zen, &mut session);
        assert_eq!(
            zen.phase,
            zen::ZenPhase::Artifact {
                index: 1,
                scroll: 0
            }
        );
        press(KeyCode::Char('h'), &mut zen, &mut session);
        assert_eq!(
            zen.phase,
            zen::ZenPhase::Artifact {
                index: 0,
                scroll: 0
            }
        );
        // The viewer is modal: normal keys are swallowed, not fallen through.
        press(KeyCode::Char('c'), &mut zen, &mut session);
        press(KeyCode::Esc, &mut zen, &mut session);
        assert_eq!(zen.phase, zen::ZenPhase::Focus);
    }

    #[test]
    fn mouse_wheel_scrolls_zen_artifact_viewer() {
        let mut session = zen_session_with_glance();
        let mut zen = ZenState::new(&session, &[]).unwrap();
        zen.phase = zen::ZenPhase::Artifact {
            index: 0,
            scroll: 0,
        };
        let mut tui_state = TuiState {
            zen: Some(zen),
            ..TuiState::default()
        };

        assert!(handle_zen_mouse_event(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 10,
                row: 10,
                modifiers: KeyModifiers::empty(),
            },
            ratatui::prelude::Size::new(80, 24),
            &mut session,
            &mut tui_state,
        ));
        assert_eq!(
            tui_state.zen.as_ref().unwrap().phase,
            zen::ZenPhase::Artifact {
                index: 0,
                scroll: 3
            }
        );

        assert!(handle_zen_mouse_event(
            MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 10,
                row: 10,
                modifiers: KeyModifiers::empty(),
            },
            ratatui::prelude::Size::new(80, 24),
            &mut session,
            &mut tui_state,
        ));
        assert_eq!(
            tui_state.zen.as_ref().unwrap().phase,
            zen::ZenPhase::Artifact {
                index: 0,
                scroll: 0
            }
        );
    }

    #[test]
    fn mouse_wheel_scrolls_zen_focus_snippet() {
        let mut session = zen_session_with_glance();
        let original_cursor = session.diff_cursor;
        let zen = ZenState::new(&session, &[]).unwrap();
        let mut tui_state = TuiState {
            zen: Some(zen),
            ..TuiState::default()
        };

        assert!(handle_zen_mouse_event(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 10,
                row: 10,
                modifiers: KeyModifiers::empty(),
            },
            ratatui::prelude::Size::new(80, 24),
            &mut session,
            &mut tui_state,
        ));
        assert_eq!(session.focus, Focus::Diff);
        assert!(session.diff_cursor > original_cursor);
    }

    #[test]
    fn zen_e_without_artifacts_notices_instead_of_opening() {
        let mut session = zen_session_with_glance();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let zen_backend = MockJjBackend::with_diff(Ok(String::new()));
        let mut tui_state = TuiState::default();
        let mut zen = ZenState::new(&session, &[]).unwrap();

        assert!(matches!(
            handle_zen_key(
                KeyEvent::from(KeyCode::Char('e')),
                &mut zen,
                &mut session,
                &keymap,
                &zen_loader(&zen_backend),
                &mut tui_state,
            ),
            ZenKeyOutcome::Consumed
        ));
        assert_eq!(zen.phase, zen::ZenPhase::Focus);
        assert!(
            tui_state
                .notice
                .unwrap()
                .message
                .contains("no artifacts on this stop")
        );
    }

    #[test]
    fn glance_board_enter_jumps_to_the_entry_and_ends_zen() {
        let mut session = zen_session_with_glance();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let zen_backend = MockJjBackend::with_diff(Ok(String::new()));
        let mut tui_state = TuiState::default();
        let mut zen = ZenState::new(&session, &[]).unwrap();
        zen.phase = zen::ZenPhase::Glance;

        assert!(matches!(
            handle_zen_key(
                KeyEvent::from(KeyCode::Enter),
                &mut zen,
                &mut session,
                &keymap,
                &zen_loader(&zen_backend),
                &mut tui_state,
            ),
            ZenKeyOutcome::End { .. }
        ));
        assert_eq!(session.selected_file().unwrap().path, "b.rs");
        assert!(tui_state.notice.unwrap().message.contains("jumped to b.rs"));
    }

    #[test]
    fn glance_board_swallows_normal_mode_keys() {
        let mut session = zen_session_with_glance();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let zen_backend = MockJjBackend::with_diff(Ok(String::new()));
        let mut tui_state = TuiState::default();
        let mut zen = ZenState::new(&session, &[]).unwrap();
        zen.phase = zen::ZenPhase::Glance;

        // 'c' would open a comment editor in normal mode; the board is a
        // bulk-skim screen, so it must not fire invisibly underneath.
        assert!(matches!(
            handle_zen_key(
                KeyEvent::from(KeyCode::Char('c')),
                &mut zen,
                &mut session,
                &keymap,
                &zen_loader(&zen_backend),
                &mut tui_state,
            ),
            ZenKeyOutcome::Consumed
        ));
    }

    /// A session where chunk two is anchored to a stack change `bbb`.
    fn stacked_zen_session() -> ReviewSession {
        let mut session = zen_session();
        session.apply_agent_overlay(&crate::agent::AgentOverlay {
            chunks: vec![
                crate::agent::ReviewChunk {
                    id: "c1".to_owned(),
                    title: "home stop".to_owned(),
                    importance: crate::agent::ChunkImportance::Spotlight,
                    change_id: None,
                    rationale: None,
                    explanation: None,
                    artifacts: Vec::new(),
                    parts: vec![crate::agent::ChunkPart {
                        path: "a.rs".to_owned(),
                        start_line: Some(1),
                        end_line: Some(1),
                    }],
                },
                crate::agent::ReviewChunk {
                    id: "c2".to_owned(),
                    title: "stacked stop".to_owned(),
                    importance: crate::agent::ChunkImportance::Spotlight,
                    change_id: Some("bbb".to_owned()),
                    rationale: None,
                    explanation: Some("The second change of the stack.".to_owned()),
                    artifacts: Vec::new(),
                    parts: vec![crate::agent::ChunkPart {
                        path: "b.rs".to_owned(),
                        start_line: Some(1),
                        end_line: Some(1),
                    }],
                },
            ],
            ..Default::default()
        });
        session
    }

    #[test]
    fn zen_retargets_to_a_change_anchored_stop_without_going_stale() {
        let mut session = stacked_zen_session();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let zen_backend = MockJjBackend::with_diff(Ok(r#"diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1 +1 @@
-old
+new
"#
        .to_owned()));
        let mut tui_state = TuiState::default();
        let mut zen = ZenState::new(&session, &[]).unwrap();
        session.file_pane_visible = false;
        // [home chapter, home stop, bbb chapter, bbb stop]: the chapter
        // card for bbb already retargets the review to that change.
        assert_eq!(zen.stops.len(), 4);

        // Advancing to the bbb-anchored stop retargets the review to that
        // change's own diff (stacked-PR style) without ending zen.
        for _ in 0..3 {
            assert!(matches!(
                handle_zen_key(
                    KeyEvent::from(KeyCode::Enter),
                    &mut zen,
                    &mut session,
                    &keymap,
                    &zen_loader(&zen_backend),
                    &mut tui_state,
                ),
                ZenKeyOutcome::Consumed
            ));
        }
        assert_eq!(session.target, ReviewTarget::new("bbb-", "bbb"));
        assert!(!zen.is_stale(&session));
        assert!(!session.file_pane_visible);
        assert_eq!(session.zen_focus.as_ref().unwrap().path, "b.rs");
        assert_eq!(
            zen_backend.calls.borrow().as_slice(),
            [ReviewTarget::new("bbb-", "bbb")]
        );

        // Ending the walkthrough returns to the home target.
        tui_state.zen = Some(zen);
        let mut mode = Mode::Normal;
        handle_key_event(
            KeyEvent::from(KeyCode::Esc),
            &mut session,
            &mut mode,
            &keymap,
            &zen_loader(&zen_backend),
            &mut tui_state,
        )
        .unwrap();
        assert!(tui_state.zen.is_none());
        assert_eq!(session.target, ReviewTarget::trunk_to_current());
        assert_eq!(
            zen_backend.calls.borrow().as_slice(),
            [
                ReviewTarget::new("bbb-", "bbb"),
                ReviewTarget::trunk_to_current(),
            ]
        );
    }

    #[test]
    fn zen_stays_put_when_a_change_anchored_stop_fails_to_load() {
        let mut session = stacked_zen_session();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let zen_backend = MockJjBackend::with_diff(Err("boom".to_owned()));
        let mut tui_state = TuiState::default();
        let mut zen = ZenState::new(&session, &[]).unwrap();

        // Step off the home chapter onto the home stop, then try to enter
        // the bbb chapter (which needs its change's diff).
        for _ in 0..2 {
            assert!(matches!(
                handle_zen_key(
                    KeyEvent::from(KeyCode::Enter),
                    &mut zen,
                    &mut session,
                    &keymap,
                    &zen_loader(&zen_backend),
                    &mut tui_state,
                ),
                ZenKeyOutcome::Consumed
            ));
        }

        // The load failed: still on the home stop, target unchanged, and
        // the notice explains what happened.
        assert_eq!(zen.index, 1);
        assert_eq!(session.target, ReviewTarget::trunk_to_current());
        let notice = tui_state.notice.unwrap();
        assert_eq!(notice.level, UiNoticeLevel::Error);
        assert!(notice.message.contains("bbb"));
    }

    const REFRESHED_DIFF: &str = r#"diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1 +1 @@
-old
+newer
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1 +1 @@
-old
+new
diff --git a/c.rs b/c.rs
--- a/c.rs
+++ b/c.rs
@@ -1 +1 @@
-old
+new
"#;

    #[test]
    fn repo_polling_baselines_then_refreshes_in_place_on_change() {
        let mut session = zen_session();
        session.file_pane_visible = false;
        let backend = MockJjBackend::with_diff(Ok(REFRESHED_DIFF.to_owned()));
        let loader = zen_loader(&backend);
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
    fn repo_polling_notice_and_activity_include_specific_events() {
        let mut session = zen_session();
        session.files[0].viewed = true;
        let backend = MockJjBackend::with_diff(Ok(REFRESHED_DIFF.to_owned()));
        let loader = zen_loader(&backend);
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

    #[test]
    fn live_refresh_runs_under_read_only_popups() {
        assert!(mode_allows_live_refresh(&Mode::Normal));
        assert!(mode_allows_live_refresh(&Mode::Activity(
            ActivityListState::new()
        )));
        assert!(mode_allows_live_refresh(&Mode::Help));
    }

    #[test]
    fn repo_polling_is_throttled_and_ignores_fingerprint_errors() {
        let mut session = zen_session();
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = zen_loader(&backend);
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
    fn persistent_fingerprint_failures_surface_an_error_then_recovery() {
        let mut session = zen_session();
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = zen_loader(&backend);
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
    fn refresh_keeps_an_active_zen_walkthrough_alive() {
        let dir = tempfile::tempdir().unwrap();
        let overlay_path = dir.path().join("agent.json");
        let mut session = zen_session();
        let overlay = crate::agent::AgentOverlay {
            chunks: session.review_chunks.clone(),
            ..Default::default()
        };
        overlay.save(&overlay_path).unwrap();
        let backend = MockJjBackend::with_diff(Ok(REFRESHED_DIFF.to_owned()));
        let loader = zen_loader(&backend);
        let mut tui_state = TuiState {
            agent_overlay_path: Some(overlay_path),
            ..TuiState::default()
        };
        let mut zen = ZenState::new(&session, &[]).unwrap();
        zen.advance(); // chapter card -> first stop
        zen.advance(); // -> second stop (b.rs)
        tui_state.zen = Some(zen);

        maybe_refresh_review(&loader, &mut session, &mut tui_state);
        *backend.fingerprint.borrow_mut() = Ok("changed".to_owned());
        tui_state.last_repo_poll = None;
        maybe_refresh_review(&loader, &mut session, &mut tui_state);

        // The walkthrough survived the reload: stops rebuilt from the
        // reapplied overlay chunks, position kept, staleness key updated.
        let zen = tui_state.zen.as_ref().unwrap();
        assert_eq!(zen.source, zen::ZenSource::Curated);
        assert_eq!(zen.index, 2);
        assert!(!zen.is_stale(&session));
        assert!(!session.review_chunks.is_empty());
        assert_eq!(session.zen_focus.as_ref().unwrap().path, "b.rs");
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
    fn starting_zen_with_no_files_shows_a_notice() {
        let mut session = snapshot_session("");
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let mut mode = Mode::Normal;

        handle_normal_action(
            Action::Zen,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();

        assert!(matches!(mode, Mode::Normal));
        assert!(tui_state.zen.is_none());
        assert!(
            tui_state
                .notice
                .unwrap()
                .message
                .contains("nothing to review")
        );
    }

    #[test]
    fn starting_zen_without_chunks_tours_files_and_hides_the_pane() {
        let mut session = zen_session();
        session.review_chunks.clear();
        session.sessions.clear();
        let backend = MockJjBackend::with_diff(Ok(String::new()));
        let loader = ReviewLoader {
            ignore_globs: Vec::new(),
            generated_matcher: GeneratedMatcher::new(&Default::default()).unwrap(),
            jj: &backend,
        };
        let mut tui_state = TuiState::default();
        let mut mode = Mode::Normal;

        handle_normal_action(
            Action::Zen,
            &mut session,
            &mut mode,
            &loader,
            &mut tui_state,
        )
        .unwrap();

        assert!(matches!(mode, Mode::Normal));
        let zen = tui_state.zen.as_ref().unwrap();
        assert_eq!(zen.stops.len(), 3); // opening chapter + one stop per file
        assert!(zen.restore_file_pane);
        assert!(!session.file_pane_visible);
        // Zen lands on the opening chapter card: nothing framed yet, parked
        // at the top of the change.
        assert!(session.zen_focus.is_none());
        assert_eq!(session.selected_file().unwrap().path, "a.rs");
        assert!(
            tui_state
                .notice
                .unwrap()
                .message
                .contains("touring 2 file(s)")
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
}
