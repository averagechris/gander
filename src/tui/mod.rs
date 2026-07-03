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

use std::{
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
    agent::AgentProcess,
    app::{Focus, ReviewSession},
    config::{AgentConfig, KeybindingsConfig},
    diff::DiffSet,
    generated::GeneratedMatcher,
    jj::{JjBackend, ReviewTarget},
};

use chooser::TargetChooserState;
use chunks::ChunkListState;
use comments::CommentListState;
use drafts::DraftListState;
use editor::CommentEditor;
use flags::FlagListState;
use helpers::JjHelperState;
use keymap::{Action, KeyMap};
use ops::OperationPickerState;
use outline::SymbolOutlineState;
use render::{draw, inner_bordered, point_in_rect, row_in_inner, ui_layout};
use revset::RevsetInputState;
use search::FileSearchState;

enum Mode {
    Normal,
    Help,
    TargetChooser(TargetChooserState),
    RevsetInput(RevsetInputState),
    OperationPicker(OperationPickerState),
    JjHelpers(JjHelperState),
    FlagList(FlagListState),
    ChunkList(ChunkListState),
    DraftList(DraftListState),
    FileSearch(FileSearchState),
    SymbolOutline(SymbolOutlineState),
    CommentList(CommentListState),
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
    diff_drag: Option<DiffDrag>,
    notice: Option<UiNotice>,
    /// Fingerprint of the last autosaved files/comments payload, used to skip
    /// redundant writes between events.
    last_autosave: Option<String>,
    /// Modification time of the agent overlay at the last poll, so agent
    /// suggestions written mid-session are picked up without reloading on
    /// every tick.
    overlay_mtime: Option<std::time::SystemTime>,
    /// Where the agent overlay lives, for writing draft dispositions back.
    agent_overlay_path: Option<PathBuf>,
    /// How to summon a review agent (from `[agent]` config).
    agent_config: AgentConfig,
    /// A summoned agent, if any. Killed on drop so quitting cannot leak it.
    agent_process: Option<AgentProcess>,
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

#[allow(clippy::too_many_arguments)]
pub fn run(
    session: &mut ReviewSession,
    keybindings: &KeybindingsConfig,
    ignore_globs: Vec<String>,
    generated_matcher: GeneratedMatcher,
    jj: &dyn JjBackend,
    state_path: Option<PathBuf>,
    agent_overlay_path: Option<PathBuf>,
    acp_socket_path: Option<PathBuf>,
    agent_config: AgentConfig,
) -> Result<()> {
    let keymap = KeyMap::try_from(keybindings)?;
    let review_loader = ReviewLoader {
        ignore_globs,
        generated_matcher,
        jj,
    };

    // Host a live ACP endpoint so agents can talk to this session while the
    // human reviews. Failure to bind (e.g. another gander TUI on the same
    // repo) degrades to overlay-file collaboration with a footer notice.
    #[cfg(unix)]
    let (mut acp_bridge, acp_notice) = match (acp_socket_path, &agent_overlay_path) {
        (Some(socket_path), Some(overlay_path)) => {
            match crate::acp::socket::AcpBridge::bind(socket_path, overlay_path.clone()) {
                Ok(bridge) => (Some(bridge), None),
                Err(error) => (None, Some(format!("acp socket unavailable: {error}"))),
            }
        }
        _ => (None, None),
    };
    #[cfg(not(unix))]
    let acp_notice: Option<String> = {
        let _ = acp_socket_path;
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
        last_autosave: Some(state_fingerprint(session)),
        agent_overlay_path: agent_overlay_path.clone(),
        agent_config,
        notice: acp_notice.map(|message| UiNotice {
            level: UiNoticeLevel::Info,
            message,
        }),
        ..TuiState::default()
    };
    if tui_state.agent_config.autostart && tui_state.agent_config.command.is_some() {
        summon_agent(session, &mut tui_state);
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
        maybe_reload_agent_overlay(session, overlay_path, tui_state, false);
    }
    loop {
        // Answer queued agent requests against the live session before
        // drawing so their effects render this frame.
        #[cfg(unix)]
        if let Some(bridge) = acp_bridge.as_deref_mut()
            && bridge.process_pending(session)
        {
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

        terminal.draw(|frame| draw(frame, session, mode, keymap, tui_state.notice.as_ref()))?;

        if !event::poll(Duration::from_millis(150))? {
            // Idle ticks are the natural moment to pick up agent overlay
            // writes without competing with user input handling.
            if let Some(overlay_path) = agent_overlay_path {
                maybe_reload_agent_overlay(session, overlay_path, tui_state, true);
            }
            notice_agent_exit(tui_state);
            continue;
        }

        match event::read()? {
            Event::Key(key)
                if handle_key_event(key, session, mode, keymap, review_loader, tui_state)? =>
            {
                break;
            }
            Event::Key(_) => {}
            Event::Mouse(mouse) => {
                handle_mouse_event(mouse, terminal.size()?, session, mode, tui_state)
            }
            _ => {}
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
/// the command is any CLI that accepts a prompt. Output is logged to
/// `.gander/agent.log`; suggestions arrive through ACP like any other agent.
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
    let prompt = crate::agent::review_prompt(
        tui_state.agent_config.prompt.as_deref(),
        &session.repo,
        &session.target.base,
        &session.target.rev,
    );
    let log_path = AgentProcess::default_log_path(&session.repo);
    match AgentProcess::spawn(&session.repo, &command, &prompt, &log_path) {
        Ok(process) => {
            tui_state.agent_process = Some(process);
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: "agent summoned (output: .gander/agent.log)".to_owned(),
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
                message: format!("agent exited with {status} (see .gander/agent.log)"),
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
            session.apply_agent_overlay(&overlay);
            if notify {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "agent suggestions updated".to_owned(),
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

/// Cheap change-detection payload: only the persistable parts of the session
/// (viewed marks and comments), excluding volatile metadata like `saved_at`.
fn state_fingerprint(session: &ReviewSession) -> String {
    let state = session.to_state();
    serde_json::to_string(&(&state.files, &state.comments)).unwrap_or_default()
}

fn autosave_state(session: &ReviewSession, state_path: &Path, tui_state: &mut TuiState) {
    let fingerprint = state_fingerprint(session);
    if tui_state.last_autosave.as_deref() == Some(fingerprint.as_str()) {
        return;
    }
    match session.to_state().save(state_path) {
        Ok(()) => tui_state.last_autosave = Some(fingerprint),
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
        Mode::ChunkList(list) => {
            if handle_chunk_list_key(key, list, session, keymap) {
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
            ReviewTarget::trunk_to_current(),
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
                *mode = Mode::OperationPicker(OperationPickerState::new(operations));
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
        Action::ChunkList => {
            if session.review_chunks.is_empty() {
                tui_state.notice = Some(UiNotice {
                    level: UiNoticeLevel::Info,
                    message: "no review chunks suggested".to_owned(),
                });
            } else {
                *mode = Mode::ChunkList(ChunkListState::new(session));
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

/// Step through the current stack (`trunk()..@`) change-by-change, reviewing
/// each change against its parent.
fn step_stack(
    review_loader: &ReviewLoader<'_>,
    session: &mut ReviewSession,
    delta: isize,
    tui_state: &mut TuiState,
) {
    let stack = match review_loader.jj.stack_changes(&session.repo) {
        Ok(stack) => stack,
        Err(error) => {
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Error,
                message: format!("failed to load stack: {error:?}"),
            });
            return;
        }
    };
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
        .or_else(|| (session.target.rev == "@").then(|| stack.len() - 1));
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
                &change.description
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
    if let Some(action) = keymap.target_picker_action_for(&key) {
        match action {
            Action::TargetPickerMoveDown => picker.move_selection(1),
            Action::TargetPickerMoveUp => picker.move_selection(-1),
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
        KeyCode::Char('j') | KeyCode::Down => {
            picker.move_selection(1);
            false
        }
        KeyCode::Char('k') | KeyCode::Up => {
            picker.move_selection(-1);
            false
        }
        _ => false,
    }
}

/// Compare the current diff against the same target at a prior operation:
/// unchanged files are marked viewed, changed/new files marked unviewed.
fn apply_incremental_review(
    review_loader: &ReviewLoader<'_>,
    session: &mut ReviewSession,
    operation: &crate::jj::JjOperationSummary,
    tui_state: &mut TuiState,
) {
    let prior = review_loader
        .jj
        .diff_at_operation(&session.repo, &session.target, &operation.operation_id)
        .and_then(|diff_text| DiffSet::parse(&diff_text));
    let prior = match prior {
        Ok(prior) => prior,
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
    let prior_fingerprints: std::collections::BTreeMap<String, String> = prior
        .files
        .into_iter()
        .map(|file| (file.path, file.fingerprint))
        .collect();
    let (unchanged, changed) = session.apply_incremental_review(&prior_fingerprints);
    tui_state.notice = Some(UiNotice {
        level: UiNoticeLevel::Info,
        message: format!(
            "since op {}: {unchanged} unchanged file(s) marked viewed, {changed} need re-review",
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

fn handle_chunk_list_key(
    key: KeyEvent,
    list: &mut ChunkListState,
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
            if let Some(part) = list.selected_row().and_then(|row| row.part.clone()) {
                session.jump_to_chunk_part(&part);
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
        KeyCode::Char('x') => {
            if let Some(id) = list.selected_comment_id(session) {
                session.delete_comment(&id);
                list.clamp(session);
            }
            session.comments.is_empty()
        }
        _ => false,
    }
}

impl ReviewLoader<'_> {
    fn base_candidates(&self, session: &ReviewSession) -> Result<Vec<crate::jj::JjChangeSummary>> {
        self.jj.change_summaries(&session.repo)
    }

    fn load(&self, session: &mut ReviewSession, target: ReviewTarget) -> Result<()> {
        let diff_text = self
            .jj
            .diff(&session.repo, &target)
            .with_context(|| format!("failed to read jj diff for {target}"))?;
        let mut diff = DiffSet::parse(&diff_text)
            .with_context(|| format!("failed to parse jj diff for {target}"))?;
        diff.apply_ignores(&self.ignore_globs)?;
        session.replace_diff(target, diff);
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
            tui_state.notice = Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: format!("loaded {target}"),
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
    match key.code {
        KeyCode::Char(ch) => editor.insert_char(ch),
        KeyCode::Left => editor.move_left(),
        KeyCode::Right => editor.move_right(),
        KeyCode::Up => editor.move_up(),
        KeyCode::Down => editor.move_down(),
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
    if matches!(
        mode,
        Mode::Help
            | Mode::CommentInput { .. }
            | Mode::RevsetInput(_)
            | Mode::OperationPicker(_)
            | Mode::JjHelpers(_)
            | Mode::FlagList(_)
            | Mode::ChunkList(_)
            | Mode::DraftList(_)
            | Mode::FileSearch(_)
            | Mode::SymbolOutline(_)
            | Mode::CommentList(_)
    ) {
        return;
    }

    let layout = ui_layout(Rect::new(0, 0, terminal_size.width, terminal_size.height));
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

    struct MockJjBackend {
        calls: RefCell<Vec<ReviewTarget>>,
        diff_text: Result<String, String>,
        summaries: Vec<JjChangeSummary>,
        stack: Vec<JjChangeSummary>,
        operations: Vec<crate::jj::JjOperationSummary>,
        diff_at_op: Option<String>,
        commands: RefCell<Vec<Vec<String>>>,
        command_result: Result<String, String>,
    }

    impl MockJjBackend {
        fn with_diff(diff_text: Result<String, String>) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                diff_text,
                summaries: Vec::new(),
                stack: Vec::new(),
                operations: Vec::new(),
                diff_at_op: None,
                commands: RefCell::new(Vec::new()),
                command_result: Ok(String::new()),
            }
        }
    }

    impl JjBackend for MockJjBackend {
        fn diff(&self, _repo: &Path, target: &ReviewTarget) -> Result<String> {
            self.calls.borrow_mut().push(target.clone());
            match &self.diff_text {
                Ok(diff_text) => Ok(diff_text.clone()),
                Err(error) => bail!(error.clone()),
            }
        }

        fn change_summaries(&self, _repo: &Path) -> Result<Vec<JjChangeSummary>> {
            Ok(self.summaries.clone())
        }

        fn stack_changes(&self, _repo: &Path) -> Result<Vec<JjChangeSummary>> {
            Ok(self.stack.clone())
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
        autosave_state(&session, &state_path, &mut tui_state);
        assert!(!state_path.exists());

        session.toggle_viewed();
        session.add_comment("note".into());
        autosave_state(&session, &state_path, &mut tui_state);

        let saved = crate::state::ReviewState::load_or_default(&state_path).unwrap();
        assert!(saved.files["a.txt"].viewed);
        assert_eq!(saved.comments.len(), 1);
        assert!(tui_state.notice.is_none());

        // Unchanged session: fingerprint short-circuits the write.
        let modified_before = std::fs::metadata(&state_path).unwrap().modified().unwrap();
        autosave_state(&session, &state_path, &mut tui_state);
        let modified_after = std::fs::metadata(&state_path).unwrap().modified().unwrap();
        assert_eq!(modified_before, modified_after);
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
        let mut session = snapshot_session(
            r#"diff --git a/same.rs b/same.rs
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
"#,
        );
        let same_file_diff = session
            .files
            .iter()
            .find(|file| file.path == "same.rs")
            .unwrap()
            .diff
            .raw
            .clone();
        let mut backend = MockJjBackend::with_diff(Ok(String::new()));
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
        assert!(same.viewed);
        assert!(!changed.viewed);
        let notice = tui_state.notice.unwrap();
        assert!(notice.message.contains("since op op123"));
        assert!(notice.message.contains("1 unchanged"));
        assert!(notice.message.contains("1 need re-review"));
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

        // No overlay on disk yet: nothing happens.
        maybe_reload_agent_overlay(&mut session, &overlay_path, &mut tui_state, true);
        assert!(session.agent_ordering.is_empty());
        assert!(tui_state.notice.is_none());

        crate::agent::AgentOverlay {
            ordering: vec!["b.rs".to_owned()],
            ..Default::default()
        }
        .save(&overlay_path)
        .unwrap();

        maybe_reload_agent_overlay(&mut session, &overlay_path, &mut tui_state, true);
        assert_eq!(session.agent_ordering, ["b.rs"]);
        assert_eq!(
            tui_state.notice.as_ref().unwrap().message,
            "agent suggestions updated"
        );

        // Unchanged mtime: no re-notification.
        tui_state.notice = None;
        maybe_reload_agent_overlay(&mut session, &overlay_path, &mut tui_state, true);
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
