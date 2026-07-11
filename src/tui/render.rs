//! All drawing code: panes, popups, styles, and layout math.

use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    ops::Range,
    rc::Rc,
};

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap,
    },
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::{DiffRow, DiffRowKind, Focus, ReviewSession, SplitRow, split_rows},
    config::DiffViewModeConfig,
    diff::DiffLineKind,
    file_tree::{FlatTreeRow, FlatTreeRowKind},
    jj::JjChangeSummary,
    state::{Comment, ReviewTarget},
    syntax::{HighlightKind, SyntaxSpan, SyntaxThemeConfig},
};

use super::{
    ActivityListState, CommentInputTarget, Mode, TuiState, UiNotice, UiNoticeLevel,
    action_items::{OpenWorkListState, OpenWorkRow},
    chooser::TargetChooserState,
    comments::CommentListState,
    drafts::DraftListState,
    editor::CommentEditor,
    flags::FlagListState,
    helpers::{JjHelperOption, JjHelperState},
    keymap::{Action, KeyMap},
    ops::OperationPickerState,
    outline::SymbolOutlineState,
    revset::{RevsetField, RevsetInputState},
    search::FileSearchState,
    text_layout::VisualTextLayout,
    view_options::{ViewOption, ViewOptionsState},
    walkthroughs::WalkthroughListState,
    zen::{ZenState, ZenStop},
};

use super::zen::ZenPhase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UiLayout {
    pub(super) files: Rect,
    pub(super) diff: Rect,
    pub(super) footer: Rect,
}

struct FooterContext<'a> {
    mode: &'a Mode,
    keymap: &'a KeyMap,
    identity_chip: Option<&'a str>,
    notice: Option<&'a UiNotice>,
    zen: Option<&'a ZenState>,
}

fn footer_context<'a>(
    mode: &'a Mode,
    keymap: &'a KeyMap,
    tui_state: &'a TuiState,
    notice: Option<&'a UiNotice>,
    zen: Option<&'a ZenState>,
) -> FooterContext<'a> {
    FooterContext {
        mode,
        keymap,
        identity_chip: tui_state.current_identity_chip.as_deref(),
        notice,
        zen,
    }
}

pub(super) fn draw(
    frame: &mut ratatui::Frame<'_>,
    session: &ReviewSession,
    mode: &Mode,
    keymap: &KeyMap,
    tui_state: &TuiState,
    notice: Option<&UiNotice>,
    zen: Option<&ZenState>,
) {
    let layout = ui_layout(frame.area(), session.file_pane_visible);

    // The zen focus card and glance board are full-screen takeovers: the
    // point is a single object of attention, not panes. The reading phase
    // keeps the normal panes with out-of-range rows dimmed.
    match zen.map(|zen| zen.phase) {
        Some(ZenPhase::Focus) => {
            let body = body_area(frame.area());
            draw_zen_focus(frame, body, session, zen.expect("checked above"), keymap);
            draw_footer(
                frame,
                layout.footer,
                session,
                &footer_context(mode, keymap, tui_state, notice, zen),
            );
        }
        Some(ZenPhase::Artifact { index, scroll }) => {
            let body = body_area(frame.area());
            let zen = zen.expect("checked above");
            draw_zen_focus(frame, body, session, zen, keymap);
            draw_zen_artifact(frame, body, zen, index, scroll, keymap);
            draw_footer(
                frame,
                layout.footer,
                session,
                &footer_context(mode, keymap, tui_state, notice, Some(zen)),
            );
        }
        Some(ZenPhase::Glance) => {
            let body = body_area(frame.area());
            draw_zen_glance(frame, body, session, zen.expect("checked above"), keymap);
            draw_footer(
                frame,
                layout.footer,
                session,
                &footer_context(mode, keymap, tui_state, notice, zen),
            );
        }
        _ => {
            if session.file_pane_visible {
                draw_files(frame, layout.files, session);
            }
            draw_diff(frame, layout.diff, session, tui_state);
            draw_footer(
                frame,
                layout.footer,
                session,
                &footer_context(mode, keymap, tui_state, notice, zen),
            );

            // The zen reading panel is a layer under any popup: progress and
            // rationale stay visible while e.g. a comment is being written.
            if let Some(zen) = zen {
                draw_zen_panel(frame, frame.area(), session, zen, keymap);
            }
        }
    }

    match mode {
        Mode::TargetChooser(chooser) => {
            draw_target_chooser_popup(frame, frame.area(), chooser, keymap)
        }
        Mode::RevsetInput(input) => draw_revset_input_popup(frame, frame.area(), input, keymap),
        Mode::OperationPicker(picker) => {
            draw_operation_picker_popup(frame, frame.area(), picker, keymap)
        }
        Mode::JjHelpers(state) => draw_jj_helpers_popup(frame, frame.area(), state, keymap),
        Mode::FlagList(list) => draw_flag_list_popup(frame, frame.area(), list, keymap),
        Mode::OpenWork(list) => draw_open_work_popup(frame, frame.area(), session, list, keymap),
        Mode::Activity(list) => draw_activity_popup(frame, frame.area(), tui_state, list),
        Mode::WalkthroughList(list) => {
            draw_walkthrough_list_popup(frame, frame.area(), session, list, keymap)
        }
        Mode::DraftList(list) => draw_draft_list_popup(frame, frame.area(), list, keymap),
        Mode::FileSearch(search) => draw_file_search_popup(frame, frame.area(), search, keymap),
        Mode::SymbolOutline(outline) => {
            draw_symbol_outline_popup(frame, frame.area(), outline, keymap)
        }
        Mode::CommentList(list) => {
            draw_comment_list_popup(frame, frame.area(), session, list, keymap)
        }
        Mode::ViewOptions(state) => {
            draw_view_options_popup(frame, frame.area(), session, state, keymap)
        }
        Mode::CommentInput { editor, target } => {
            draw_comment_popup(frame, frame.area(), session, editor, target, keymap)
        }
        Mode::Help => draw_help_popup(frame, frame.area(), keymap, tui_state.help_scroll),
        Mode::Normal => {}
    }
}

pub(super) fn ui_layout(area: Rect, files_visible: bool) -> UiLayout {
    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(2)])
        .split(area);
    let files_width = if files_visible { 44 } else { 0 };
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(files_width), Constraint::Min(40)])
        .split(main[0]);

    UiLayout {
        files: body[0],
        diff: body[1],
        footer: main[1],
    }
}

/// Everything above the two-line footer: the canvas for zen's full-screen
/// surfaces.
fn body_area(area: Rect) -> Rect {
    Rect {
        height: area.height.saturating_sub(2),
        ..area
    }
}

pub(super) fn inner_bordered(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

pub(super) fn point_in_rect(x: u16, y: u16, rect: Rect) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

pub(super) fn row_in_inner(y: u16, inner: Rect) -> Option<usize> {
    (y >= inner.y && y < inner.y.saturating_add(inner.height)).then_some((y - inner.y) as usize)
}

fn draw_files(frame: &mut ratatui::Frame<'_>, area: Rect, session: &ReviewSession) {
    let tree = session.file_tree();
    if tree.rows.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from("No changed files"),
                Line::from(""),
                Line::from("Try t for trunk, p for parent, b for target chooser, or adjust --base/--rev/--ignore."),
            ])
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title("files"))
            .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let items: Vec<ListItem<'_>> = tree
        .rows
        .iter()
        .map(|row| match &row.kind {
            FlatTreeRowKind::Directory { collapsed } => render_directory_row(row, *collapsed),
            FlatTreeRowKind::File { file_index } => render_file_row(row, session, *file_index),
        })
        .collect();

    let mut state = ListState::default().with_selected(session.selected_tree_row(&tree));
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("files"))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_directory_row(row: &FlatTreeRow, collapsed: bool) -> ListItem<'static> {
    let indent = "  ".repeat(row.depth.min(8));
    let glyph = if collapsed { " ▸ " } else { " ▾ " };
    ListItem::new(Line::from(vec![
        Span::raw(indent),
        Span::styled(row.stats.mark(), Style::default().fg(Color::Green)),
        Span::raw(glyph),
        Span::styled(
            row.label.clone(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {}/{}", row.stats.viewed, row.stats.total),
            Style::default().fg(Color::DarkGray),
        ),
    ]))
}

fn render_file_row(
    row: &FlatTreeRow,
    session: &ReviewSession,
    file_index: usize,
) -> ListItem<'static> {
    let file = &session.files[file_index];
    let reviewed = file.viewed || file.caught_up;
    let (mark, mark_style) = if file.changed_since_look && reviewed {
        ("~", Style::default().fg(Color::Yellow))
    } else if file.changed_since_look {
        (
            "±",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else if file.viewed_stale {
        ("~", Style::default().fg(Color::Yellow))
    } else if file.viewed {
        ("✓", Style::default().fg(Color::Green))
    } else if file.caught_up {
        ("◌", Style::default().fg(Color::DarkGray))
    } else {
        ("•", Style::default().fg(Color::Green))
    };
    let style = if reviewed {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };
    let flag_span = if session.file_has_flags(&file.path) {
        Span::styled(
            "!",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw(" ")
    };
    ListItem::new(Line::from(vec![
        Span::raw("  ".repeat(row.depth.min(8))),
        Span::styled(mark, mark_style),
        flag_span,
        Span::styled(
            format!("{:>7}", file.status),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(" "),
        Span::styled(
            if file.generated { "gen " } else { "    " },
            Style::default().fg(Color::Magenta),
        ),
        Span::styled(row.label.clone(), style),
    ]))
}

fn draw_diff(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    tui_state: &TuiState,
) {
    if session.selected_visible_file().is_none() {
        let generated_hint = if session.hide_generated {
            "Noisy/generated files are hidden. Press the hide-noisy toggle to show them."
        } else {
            "If files disappeared unexpectedly, check --ignore filters."
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "No changed files",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(format!("Current target: {}", session.target)),
                Line::from(""),
                Line::from(
                    "Use t for launch target, p for @-..@, b for chooser, or pass --base/--rev.",
                ),
                Line::from(generated_hint),
            ])
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(diff_pane_title(session)),
            )
            .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }

    let rows = session.diff_rows_for_selected_file();
    let inner = inner_bordered(area);
    let split_requested = session.diff_cues.view == DiffViewModeConfig::SideBySide;
    let split_active = split_requested && inner.width >= MIN_SPLIT_WIDTH;
    let measured = cached_diff_layout(session, rows.clone(), inner, split_active, tui_state);
    let start = measured.viewport_start(session, inner.height as usize);
    let lines = materialize_diff_window(session, &rows, &measured, start, inner.height as usize);

    let mut title = diff_pane_title(session);
    if split_requested && !split_active {
        // Two unreadable half-panes help nobody: fall back to unified on
        // narrow terminals and say so in the title.
        title.push_str(" · unified (narrow)");
    }
    let horizontal = measured.effective_horizontal_scroll(session);
    if !session.diff_cues.soft_wrap && horizontal > 0 {
        title.push_str(&format!(" · x:{horizontal}"));
    }
    let paragraph =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(paragraph, area);
}

/// Minimum inner width (columns) for the side-by-side layout.
const MIN_SPLIT_WIDTH: u16 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffVisualHit {
    Full(usize),
    Split {
        left: Option<usize>,
        right: Option<usize>,
        divider: usize,
    },
}

impl DiffVisualHit {
    fn contains(self, row: usize) -> bool {
        match self {
            Self::Full(owner) => owner == row,
            Self::Split { left, right, .. } => left == Some(row) || right == Some(row),
        }
    }

    fn row_at(self, column: usize) -> Option<usize> {
        match self {
            Self::Full(row) => Some(row),
            Self::Split {
                left,
                right,
                divider,
            } => {
                if column < divider {
                    left
                } else {
                    right
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffChrome {
    Full,
    Prefix,
    None,
}

impl DiffChrome {
    fn width(self, line_number_width: usize) -> usize {
        match self {
            Self::Full => line_number_width + 4,
            Self::Prefix => 2,
            Self::None => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffCellVisual {
    row: usize,
    lineno: Option<usize>,
    chrome: DiffChrome,
    content_range: Option<Range<usize>>,
    continuation: bool,
    width: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiffVisualSource {
    Plain {
        row: usize,
        byte_range: Option<Range<usize>>,
        width: usize,
    },
    Diff(DiffCellVisual),
    Split {
        left: Option<DiffCellVisual>,
        right: Option<DiffCellVisual>,
        left_width: usize,
        right_width: usize,
    },
    Comment {
        owner: usize,
        comment_index: usize,
        byte_range: Range<usize>,
        width: usize,
    },
}

#[derive(Debug, Clone)]
struct DiffVisualLine {
    source: DiffVisualSource,
    hit: DiffVisualHit,
    block_anchor: usize,
    is_comment: bool,
}

#[derive(Debug, Clone, Default)]
struct MeasuredDiffLayout {
    lines: Vec<DiffVisualLine>,
    line_number_width: usize,
    width: usize,
    horizontal_limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffLayoutCacheKey {
    rows_ptr: usize,
    width: u16,
    split_active: bool,
    soft_wrap: bool,
    annotation_hash: u64,
}

#[derive(Debug, Clone)]
struct CachedDiffLayout {
    key: DiffLayoutCacheKey,
    _rows: Rc<Vec<DiffRow>>,
    layout: Rc<MeasuredDiffLayout>,
}

#[derive(Debug, Default)]
pub(super) struct DiffLayoutCache {
    entry: Option<CachedDiffLayout>,
    #[cfg(test)]
    builds: usize,
}

impl MeasuredDiffLayout {
    fn viewport_start(&self, session: &ReviewSession, viewport_height: usize) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        let logical = session.diff_scroll as usize;
        let first = self
            .lines
            .iter()
            .position(|line| line.hit.contains(logical))
            .or_else(|| {
                self.lines
                    .iter()
                    .position(|line| line.block_anchor >= logical)
            })
            .unwrap_or_else(|| self.lines.len().saturating_sub(1));
        let anchor = self.lines[first].block_anchor;
        let block_end = self.lines[first..]
            .iter()
            .position(|line| line.block_anchor != anchor)
            .map(|offset| first + offset)
            .unwrap_or(self.lines.len());
        let requested = first
            .saturating_add(session.diff_visual_offset)
            .min(block_end.saturating_sub(1));
        let maximum_top = self.lines.len().saturating_sub(viewport_height.max(1));
        requested.min(maximum_top)
    }

    fn effective_horizontal_scroll(&self, session: &ReviewSession) -> usize {
        if session.diff_cues.soft_wrap {
            0
        } else {
            session.diff_horizontal_scroll.min(self.horizontal_limit)
        }
    }

    fn set_viewport_from_line(&self, session: &mut ReviewSession, index: usize) {
        let index = index.min(self.lines.len().saturating_sub(1));
        let Some(line) = self.lines.get(index) else {
            session.diff_scroll = 0;
            session.diff_visual_offset = 0;
            return;
        };
        let first = self.lines[..=index]
            .iter()
            .rposition(|candidate| candidate.block_anchor != line.block_anchor)
            .map_or(0, |prior| prior + 1);
        session.diff_scroll = line.block_anchor.min(u16::MAX as usize) as u16;
        session.diff_visual_offset = index.saturating_sub(first);
    }
}

/// Build the complete terminal-row projection. Drawing, visual scrolling,
/// and mouse hit testing all consume this exact measured geometry.
fn cached_diff_layout(
    session: &ReviewSession,
    rows: Rc<Vec<DiffRow>>,
    inner: Rect,
    split_active: bool,
    tui_state: &TuiState,
) -> Rc<MeasuredDiffLayout> {
    let key = DiffLayoutCacheKey {
        rows_ptr: Rc::as_ptr(&rows) as usize,
        width: inner.width,
        split_active,
        soft_wrap: session.diff_cues.soft_wrap,
        annotation_hash: diff_annotation_hash(session),
    };
    if let Some(cached) = tui_state.diff_layout_cache.borrow().entry.as_ref()
        && cached.key == key
    {
        return Rc::clone(&cached.layout);
    }
    let layout = Rc::new(measured_diff_layout(session, &rows, inner, split_active));
    let mut cache = tui_state.diff_layout_cache.borrow_mut();
    cache.entry = Some(CachedDiffLayout {
        key,
        _rows: rows,
        layout: Rc::clone(&layout),
    });
    #[cfg(test)]
    {
        cache.builds += 1;
    }
    layout
}

fn diff_annotation_hash(session: &ReviewSession) -> u64 {
    let mut hasher = DefaultHasher::new();
    if let Some(file) = session.selected_visible_file() {
        file.path.hash(&mut hasher);
        file.fingerprint.hash(&mut hasher);
        for hunk in &file.changed_hunks {
            hunk.hash(&mut hasher);
        }
    }
    for comment in &session.comments {
        comment.id.hash(&mut hasher);
        comment.body.hash(&mut hasher);
        comment.state.label().hash(&mut hasher);
        format!("{:?}", comment.action).hash(&mut hasher);
        format!("{:?}", comment.kind).hash(&mut hasher);
        format!("{:?}", comment.anchor).hash(&mut hasher);
    }
    hasher.finish()
}

fn measured_diff_layout(
    session: &ReviewSession,
    rows: &[DiffRow],
    inner: Rect,
    split_active: bool,
) -> MeasuredDiffLayout {
    let line_number_width = measured_line_number_width(rows);
    let width = inner.width as usize;
    let chrome_width = line_number_width + 4;
    let narrowest_cell = if split_active {
        width.saturating_sub(1) / 2
    } else {
        width
    };
    let chrome = if narrowest_cell > chrome_width {
        DiffChrome::Full
    } else if narrowest_cell > 2 {
        DiffChrome::Prefix
    } else {
        DiffChrome::None
    };
    let code_width = narrowest_cell
        .saturating_sub(chrome.width(line_number_width))
        .max(1);
    let widest = rows
        .iter()
        .map(|row| UnicodeWidthStr::width(row.text.as_str()))
        .max()
        .unwrap_or(0);
    let horizontal_limit = widest.saturating_sub(code_width);
    let mut layout = if split_active {
        measured_split_layout(session, rows, width, line_number_width)
    } else {
        measured_unified_layout(session, rows, width, line_number_width)
    };
    layout.line_number_width = line_number_width;
    layout.width = width;
    layout.horizontal_limit = horizontal_limit;
    layout
}

fn measured_line_number_width(rows: &[DiffRow]) -> usize {
    rows.iter()
        .flat_map(|row| [row.old_lineno, row.new_lineno])
        .flatten()
        .map(decimal_digits)
        .max()
        .unwrap_or(4)
        .max(4)
}

fn decimal_digits(value: usize) -> usize {
    value
        .checked_ilog10()
        .map_or(1, |digits| digits as usize + 1)
}

fn measured_unified_layout(
    session: &ReviewSession,
    rows: &[DiffRow],
    width: usize,
    line_number_width: usize,
) -> MeasuredDiffLayout {
    let mut lines = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let comments = row_comment_indices(session, row);
        for source in measured_unified_row(session, row, index, width, line_number_width) {
            lines.push(DiffVisualLine {
                source,
                hit: DiffVisualHit::Full(index),
                block_anchor: index,
                is_comment: false,
            });
        }
        append_comment_lines(&mut lines, session, comments, index, index, width);
    }
    MeasuredDiffLayout {
        lines,
        ..Default::default()
    }
}

fn measured_split_layout(
    session: &ReviewSession,
    rows: &[DiffRow],
    width: usize,
    line_number_width: usize,
) -> MeasuredDiffLayout {
    let mut lines = Vec::new();
    let left_width = width.saturating_sub(1) / 2;
    let right_width = width.saturating_sub(1).saturating_sub(left_width);
    for split in split_rows(rows) {
        match split {
            SplitRow::Full(index) => {
                let comments = row_comment_indices(session, &rows[index]);
                for source in
                    measured_unified_row(session, &rows[index], index, width, line_number_width)
                {
                    lines.push(DiffVisualLine {
                        source,
                        hit: DiffVisualHit::Full(index),
                        block_anchor: index,
                        is_comment: false,
                    });
                }
                append_comment_lines(&mut lines, session, comments, index, index, width);
            }
            SplitRow::Pair { left, right } => {
                let anchor = left.or(right).unwrap_or(0);
                let left_lines =
                    measured_split_cell(session, rows, left, left_width, true, line_number_width);
                let right_lines = measured_split_cell(
                    session,
                    rows,
                    right,
                    right_width,
                    false,
                    line_number_width,
                );
                let height = left_lines.len().max(right_lines.len()).max(1);
                for visual_row in 0..height {
                    let visual_left = left_lines.get(visual_row).cloned();
                    let visual_right = right_lines.get(visual_row).cloned();
                    lines.push(DiffVisualLine {
                        source: DiffVisualSource::Split {
                            left: visual_left.clone(),
                            right: visual_right.clone(),
                            left_width,
                            right_width,
                        },
                        hit: DiffVisualHit::Split {
                            left: visual_left.map(|cell| cell.row),
                            right: visual_right.map(|cell| cell.row),
                            divider: left_width,
                        },
                        block_anchor: anchor,
                        is_comment: false,
                    });
                }
                let mut owners: Vec<_> = left.into_iter().chain(right).collect();
                owners.dedup();
                for owner in owners {
                    let comments = row_comment_indices(session, &rows[owner]);
                    append_comment_lines(&mut lines, session, comments, owner, anchor, width);
                }
            }
        }
    }
    MeasuredDiffLayout {
        lines,
        ..Default::default()
    }
}

fn row_comment_indices(session: &ReviewSession, row: &DiffRow) -> Vec<usize> {
    let Some(anchor) = row.anchor.as_ref() else {
        return Vec::new();
    };
    let comments = session.comments_for_diff_row_anchor_details(anchor);
    session
        .comments
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            comments
                .iter()
                .any(|comment| comment.id == candidate.id)
                .then_some(index)
        })
        .collect()
}

fn append_comment_lines(
    lines: &mut Vec<DiffVisualLine>,
    session: &ReviewSession,
    comments: Vec<usize>,
    owner: usize,
    block_anchor: usize,
    width: usize,
) {
    for comment_index in comments {
        let text = comment_summary_text(&session.comments[comment_index]);
        for byte_range in visual_byte_ranges(&text, width.max(1), true)
            .into_iter()
            .flatten()
        {
            lines.push(DiffVisualLine {
                source: DiffVisualSource::Comment {
                    owner,
                    comment_index,
                    byte_range,
                    width,
                },
                hit: DiffVisualHit::Full(owner),
                block_anchor,
                is_comment: true,
            });
        }
    }
}

fn measured_unified_row(
    session: &ReviewSession,
    row: &DiffRow,
    index: usize,
    width: usize,
    line_number_width: usize,
) -> Vec<DiffVisualSource> {
    if matches!(row.kind, DiffRowKind::DiffLine(_)) {
        return measured_diff_cells(
            session,
            row,
            index,
            row.new_lineno.or(row.old_lineno),
            width,
            line_number_width,
        )
        .into_iter()
        .map(DiffVisualSource::Diff)
        .collect();
    }
    let text = plain_row_text(session, row);
    visual_byte_ranges(&text, width.max(1), session.diff_cues.soft_wrap)
        .into_iter()
        .map(|byte_range| DiffVisualSource::Plain {
            row: index,
            byte_range,
            width,
        })
        .collect()
}

fn measured_split_cell(
    session: &ReviewSession,
    rows: &[DiffRow],
    cell: Option<usize>,
    width: usize,
    is_left: bool,
    line_number_width: usize,
) -> Vec<DiffCellVisual> {
    let Some(index) = cell else {
        return Vec::new();
    };
    let row = &rows[index];
    let lineno = if is_left {
        row.old_lineno
    } else {
        row.new_lineno
    };
    measured_diff_cells(session, row, index, lineno, width, line_number_width)
}

fn measured_diff_cells(
    session: &ReviewSession,
    row: &DiffRow,
    index: usize,
    lineno: Option<usize>,
    width: usize,
    line_number_width: usize,
) -> Vec<DiffCellVisual> {
    let full_chrome_width = line_number_width + 4;
    let chrome = if width > full_chrome_width {
        DiffChrome::Full
    } else if width > 2 {
        DiffChrome::Prefix
    } else {
        DiffChrome::None
    };
    let content_width = width.saturating_sub(chrome.width(line_number_width)).max(1);
    visual_byte_ranges(&row.text, content_width, session.diff_cues.soft_wrap)
        .into_iter()
        .enumerate()
        .map(|(visual_row, content_range)| DiffCellVisual {
            row: index,
            lineno,
            chrome,
            content_range,
            continuation: visual_row > 0,
            width,
        })
        .collect()
}

fn visual_byte_ranges(text: &str, width: usize, wrap: bool) -> Vec<Option<Range<usize>>> {
    if !wrap {
        return vec![None];
    }
    let layout = VisualTextLayout::read_only(text, width.max(1));
    (0..layout.rows().len())
        .map(|row| Some(layout.row_byte_range(row)))
        .collect()
}

fn plain_row_text(session: &ReviewSession, row: &DiffRow) -> String {
    match row.kind {
        DiffRowKind::FileHeader | DiffRowKind::SyntaxSummary | DiffRowKind::Raw => row.text.clone(),
        DiffRowKind::HunkHeader
            if row.hunk_index.is_some_and(|hunk| {
                session
                    .selected_file()
                    .is_some_and(|file| file.changed_hunks.contains(&hunk))
            }) =>
        {
            format!("{}  changed", row.text)
        }
        DiffRowKind::HunkHeader => row.text.clone(),
        DiffRowKind::Placeholder => format!("  \u{2298} {}", row.text),
        DiffRowKind::ContextFold | DiffRowKind::ExpandGap { .. } => {
            format!("      {}", row.text)
        }
        DiffRowKind::DiffLine(_) => row.text.clone(),
    }
}

fn comment_summary_text(comment: &Comment) -> String {
    spans_text(&comment_summary_line(comment).spans)
}

fn materialize_diff_window(
    session: &ReviewSession,
    rows: &[DiffRow],
    layout: &MeasuredDiffLayout,
    start: usize,
    height: usize,
) -> Vec<Line<'static>> {
    let horizontal = layout.effective_horizontal_scroll(session);
    let mut prepared = HashMap::new();
    layout
        .lines
        .iter()
        .skip(start)
        .take(height)
        .map(|visual| {
            materialize_diff_source_cached(
                session,
                rows,
                layout.line_number_width,
                horizontal,
                &visual.source,
                &mut prepared,
            )
        })
        .collect()
}

#[cfg(test)]
fn materialize_diff_source(
    session: &ReviewSession,
    rows: &[DiffRow],
    line_number_width: usize,
    horizontal: usize,
    source: &DiffVisualSource,
) -> Line<'static> {
    materialize_diff_source_cached(
        session,
        rows,
        line_number_width,
        horizontal,
        source,
        &mut HashMap::new(),
    )
}

fn materialize_diff_source_cached(
    session: &ReviewSession,
    rows: &[DiffRow],
    line_number_width: usize,
    horizontal: usize,
    source: &DiffVisualSource,
    prepared: &mut HashMap<(usize, Option<usize>), PreparedDiffCell>,
) -> Line<'static> {
    match source {
        DiffVisualSource::Plain {
            row, byte_range, ..
        } => {
            let comment_count = row_comment_indices(session, &rows[*row]).len();
            let spans = unified_row_line(session, &rows[*row], *row, comment_count).spans;
            let visible = byte_range.as_ref().map_or_else(
                || clip_spans(&spans, horizontal, source.width()),
                |range| slice_spans_bytes(&spans, range.clone()),
            );
            Line::from(pad_spans(visible, source.width()))
        }
        DiffVisualSource::Diff(cell) => Line::from(materialize_diff_cell(
            session,
            rows,
            line_number_width,
            horizontal,
            cell,
            prepared,
        )),
        DiffVisualSource::Split {
            left,
            right,
            left_width,
            right_width,
        } => {
            let mut spans = left.as_ref().map_or_else(
                || vec![Span::raw(" ".repeat(*left_width))],
                |cell| {
                    materialize_diff_cell(
                        session,
                        rows,
                        line_number_width,
                        horizontal,
                        cell,
                        prepared,
                    )
                },
            );
            spans.push(Span::styled(
                "\u{2502}",
                Style::default().fg(Color::DarkGray),
            ));
            spans.extend(right.as_ref().map_or_else(
                || vec![Span::raw(" ".repeat(*right_width))],
                |cell| {
                    materialize_diff_cell(
                        session,
                        rows,
                        line_number_width,
                        horizontal,
                        cell,
                        prepared,
                    )
                },
            ));
            Line::from(spans)
        }
        DiffVisualSource::Comment {
            comment_index,
            byte_range,
            ..
        } => {
            let spans = comment_summary_line(&session.comments[*comment_index]).spans;
            Line::from(pad_spans(
                slice_spans_bytes(&spans, byte_range.clone()),
                source.width(),
            ))
        }
    }
}

impl DiffVisualSource {
    fn width(&self) -> usize {
        match self {
            Self::Plain { width, .. } | Self::Comment { width, .. } => *width,
            Self::Diff(cell) => cell.width,
            Self::Split {
                left_width,
                right_width,
                ..
            } => left_width + 1 + right_width,
        }
    }
}

fn materialize_diff_cell(
    session: &ReviewSession,
    rows: &[DiffRow],
    line_number_width: usize,
    horizontal: usize,
    cell: &DiffCellVisual,
    prepared: &mut HashMap<(usize, Option<usize>), PreparedDiffCell>,
) -> Vec<Span<'static>> {
    let prepared = prepared
        .entry((cell.row, cell.lineno))
        .or_insert_with(|| prepare_diff_cell(session, rows, line_number_width, cell));
    render_prepared_diff_cell(prepared, cell, line_number_width, horizontal)
}

#[derive(Debug, Clone)]
struct PreparedDiffCell {
    chrome: Vec<Span<'static>>,
    content: Vec<Span<'static>>,
    continuation_style: Style,
}

fn prepare_diff_cell(
    session: &ReviewSession,
    rows: &[DiffRow],
    line_number_width: usize,
    cell: &DiffCellVisual,
) -> PreparedDiffCell {
    const CHROME_SPANS: usize = 5;
    let row = &rows[cell.row];
    let comments = row_comment_indices(session, row).len();
    let mut all = diff_line_cell_spans_with_width(
        session,
        row,
        cell.row,
        cell.lineno,
        comments,
        line_number_width,
    );
    if zen_row_dimmed(session, row, cell.row) {
        all = dim_spans(all);
    }
    let content = if all.len() > CHROME_SPANS {
        all.split_off(CHROME_SPANS)
    } else {
        Vec::new()
    };
    let continuation_style = diff_row_style(
        row.kind,
        session.focus == Focus::Diff && session.diff_cursor == cell.row,
        session.diff_row_in_active_range(cell.row),
    );
    PreparedDiffCell {
        chrome: all,
        content,
        continuation_style,
    }
}

fn render_prepared_diff_cell(
    prepared: &PreparedDiffCell,
    cell: &DiffCellVisual,
    line_number_width: usize,
    horizontal: usize,
) -> Vec<Span<'static>> {
    let chrome_width = cell.chrome.width(line_number_width);
    let mut spans = if cell.continuation {
        vec![Span::styled(
            " ".repeat(chrome_width),
            prepared.continuation_style,
        )]
    } else {
        match cell.chrome {
            DiffChrome::Full => prepared.chrome.clone(),
            DiffChrome::Prefix => prepared.chrome.iter().skip(3).take(2).cloned().collect(),
            DiffChrome::None => Vec::new(),
        }
    };
    let content_width = cell.width.saturating_sub(chrome_width).max(1);
    let visible = cell.content_range.as_ref().map_or_else(
        || clip_spans(&prepared.content, horizontal, content_width),
        |range| slice_spans_bytes(&prepared.content, range.clone()),
    );
    spans.extend(pad_spans(visible, content_width));
    pad_spans(spans, cell.width)
}

fn spans_text(spans: &[Span<'_>]) -> String {
    spans.iter().map(|span| span.content.as_ref()).collect()
}

fn slice_spans_bytes(spans: &[Span<'static>], range: std::ops::Range<usize>) -> Vec<Span<'static>> {
    let mut result = Vec::new();
    let mut offset = 0usize;
    for span in spans {
        let end = offset + span.content.len();
        let start = range.start.max(offset).min(end);
        let stop = range.end.max(offset).min(end);
        if start < stop {
            let local = start - offset..stop - offset;
            if span.content.is_char_boundary(local.start)
                && span.content.is_char_boundary(local.end)
            {
                result.push(Span::styled(span.content[local].to_owned(), span.style));
            }
        }
        offset = end;
        if offset >= range.end {
            break;
        }
    }
    result
}

fn clip_spans(spans: &[Span<'static>], horizontal: usize, width: usize) -> Vec<Span<'static>> {
    use unicode_segmentation::UnicodeSegmentation;
    let text = spans_text(spans);
    let mut column = 0usize;
    let end_column = horizontal.saturating_add(width);
    let mut byte_start = None;
    let mut byte_end = 0usize;
    let mut first_column = horizontal;
    for (byte, grapheme) in text.grapheme_indices(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        let next = column.saturating_add(grapheme_width);
        let wholly_visible = column >= horizontal && next <= end_column;
        if wholly_visible && (grapheme_width > 0 || column >= horizontal) {
            if byte_start.is_none() {
                byte_start = Some(byte);
                first_column = column;
            }
            byte_end = byte + grapheme.len();
        } else if column >= end_column || next > end_column {
            break;
        }
        column = next;
    }
    byte_start.map_or_else(Vec::new, |start| {
        let mut clipped = Vec::new();
        if first_column > horizontal {
            clipped.push(Span::raw(" ".repeat(first_column - horizontal)));
        }
        clipped.extend(slice_spans_bytes(spans, start..byte_end));
        clipped
    })
}

fn pad_spans(mut spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let used: usize = spans.iter().map(Span::width).sum();
    if used < width {
        spans.push(Span::raw(" ".repeat(width - used)));
    }
    spans
}

fn split_is_active(session: &ReviewSession, inner: Rect) -> bool {
    session.diff_cues.view == DiffViewModeConfig::SideBySide && inner.width >= MIN_SPLIT_WIDTH
}

pub(super) fn diff_row_at_point(
    session: &ReviewSession,
    inner: Rect,
    x: u16,
    visible_row: usize,
    tui_state: &TuiState,
) -> Option<usize> {
    let rows = session.diff_rows_for_selected_file();
    let layout = cached_diff_layout(
        session,
        rows,
        inner,
        split_is_active(session, inner),
        tui_state,
    );
    let line = layout
        .lines
        .get(layout.viewport_start(session, inner.height as usize) + visible_row)?;
    line.hit.row_at(x.saturating_sub(inner.x) as usize)
}

pub(super) fn scroll_diff_visual(
    session: &mut ReviewSession,
    inner: Rect,
    delta: isize,
    tui_state: &TuiState,
) {
    let rows = session.diff_rows_for_selected_file();
    let layout = cached_diff_layout(
        session,
        rows,
        inner,
        split_is_active(session, inner),
        tui_state,
    );
    if layout.lines.is_empty() {
        return;
    }
    let start = layout.viewport_start(session, inner.height as usize);
    let maximum_top = layout
        .lines
        .len()
        .saturating_sub(inner.height.max(1) as usize);
    let target = start.saturating_add_signed(delta).min(maximum_top);
    layout.set_viewport_from_line(session, target);
}

pub(super) fn scroll_diff_horizontal_visual(
    session: &mut ReviewSession,
    inner: Rect,
    delta: isize,
    tui_state: &TuiState,
) {
    if session.diff_cues.soft_wrap {
        return;
    }
    let rows = session.diff_rows_for_selected_file();
    let layout = cached_diff_layout(
        session,
        rows,
        inner,
        split_is_active(session, inner),
        tui_state,
    );
    let current = layout.effective_horizontal_scroll(session);
    session.diff_horizontal_scroll = current
        .saturating_add_signed(delta)
        .min(layout.horizontal_limit);
}

pub(super) fn scroll_diff_to_bottom_visual(
    session: &mut ReviewSession,
    inner: Rect,
    tui_state: &TuiState,
) {
    session.scroll_diff_to_bottom();
    let rows = session.diff_rows_for_selected_file();
    let layout = cached_diff_layout(
        session,
        rows,
        inner,
        split_is_active(session, inner),
        tui_state,
    );
    let target = layout
        .lines
        .len()
        .saturating_sub(inner.height.max(1) as usize);
    layout.set_viewport_from_line(session, target);
}

pub(super) fn ensure_diff_cursor_visible(
    session: &mut ReviewSession,
    inner: Rect,
    tui_state: &TuiState,
) {
    let rows = session.diff_rows_for_selected_file();
    let layout = cached_diff_layout(
        session,
        rows,
        inner,
        split_is_active(session, inner),
        tui_state,
    );
    let start = layout.viewport_start(session, inner.height as usize);
    let end = start.saturating_add(inner.height.max(1) as usize);
    let first = layout
        .lines
        .iter()
        .position(|line| !line.is_comment && line.hit.contains(session.diff_cursor));
    let last = layout
        .lines
        .iter()
        .rposition(|line| !line.is_comment && line.hit.contains(session.diff_cursor));
    let height = inner.height.max(1) as usize;
    match (first, last) {
        (Some(first), Some(last)) if last - first + 1 > height && first != start => {
            layout.set_viewport_from_line(session, first);
        }
        (Some(first), Some(last)) if last - first + 1 > height && first == start => {}
        (Some(first), Some(_)) if first < start => layout.set_viewport_from_line(session, first),
        (Some(_), Some(last)) if last >= end => {
            layout.set_viewport_from_line(session, last.saturating_add(1).saturating_sub(height))
        }
        _ => {}
    }
}

pub(super) fn reconcile_diff_viewport(
    session: &mut ReviewSession,
    inner: Rect,
    keep_cursor_visible: bool,
    tui_state: &TuiState,
) {
    let rows = session.diff_rows_for_selected_file();
    let layout = cached_diff_layout(
        session,
        rows,
        inner,
        split_is_active(session, inner),
        tui_state,
    );
    session.diff_horizontal_scroll = layout.effective_horizontal_scroll(session);
    let start = layout.viewport_start(session, inner.height as usize);
    layout.set_viewport_from_line(session, start);
    if keep_cursor_visible {
        ensure_diff_cursor_visible(session, inner, tui_state);
    }
}

pub(super) fn diff_cursor_is_visible(
    session: &ReviewSession,
    inner: Rect,
    tui_state: &TuiState,
) -> bool {
    let rows = session.diff_rows_for_selected_file();
    let layout = cached_diff_layout(
        session,
        rows,
        inner,
        split_is_active(session, inner),
        tui_state,
    );
    let start = layout.viewport_start(session, inner.height as usize);
    let end = start.saturating_add(inner.height.max(1) as usize);
    layout.lines[start.min(layout.lines.len())..end.min(layout.lines.len())]
        .iter()
        .any(|line| !line.is_comment && line.hit.contains(session.diff_cursor))
}

/// One full-width line for a diff row in the unified layout (also used for
/// full-width rows in the side-by-side layout).
fn unified_row_line(
    session: &ReviewSession,
    row: &DiffRow,
    index: usize,
    comment_count: usize,
) -> Line<'static> {
    let line = match row.kind {
        DiffRowKind::FileHeader => Line::from(Span::styled(
            row.text.clone(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        DiffRowKind::SyntaxSummary => Line::from(Span::styled(
            row.text.clone(),
            Style::default().fg(Color::Magenta),
        )),
        DiffRowKind::HunkHeader => Line::from(Span::styled(
            if row.hunk_index.is_some_and(|hunk| {
                session
                    .selected_file()
                    .is_some_and(|file| file.changed_hunks.contains(&hunk))
            }) {
                format!("{}  changed", row.text)
            } else {
                row.text.clone()
            },
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )),
        DiffRowKind::Raw => Line::from(row.text.clone()),
        DiffRowKind::Placeholder => Line::from(Span::styled(
            format!("  \u{2298} {}", row.text),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )),
        DiffRowKind::ContextFold | DiffRowKind::ExpandGap { .. } => Line::from(Span::styled(
            format!("      {}", row.text),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )),
        DiffRowKind::DiffLine(_) => Line::from(diff_line_cell_spans(
            session,
            row,
            index,
            row.new_lineno.or(row.old_lineno),
            comment_count,
        )),
    };
    if zen_row_dimmed(session, row, index) {
        Line::from(dim_spans(line.spans))
    } else {
        line
    }
}

/// True when the zen walkthrough frames a line range in the selected file
/// and this row falls outside it: chrome rows and out-of-range diff lines
/// recede so the current stop visually pops (docs/focused-diff-ux.md §6).
/// The cursor row never dims so it stays readable while moving around.
fn zen_row_dimmed(session: &ReviewSession, row: &DiffRow, index: usize) -> bool {
    let Some(focus) = &session.zen_focus else {
        return false;
    };
    let Some((start, end)) = focus.lines else {
        return false;
    };
    if session
        .selected_visible_file()
        .map(|file| file.path.as_str())
        != Some(focus.path.as_str())
    {
        return false;
    }
    if session.focus == Focus::Diff && session.diff_cursor == index {
        return false;
    }
    match row.kind {
        DiffRowKind::DiffLine(_) => {
            let lineno = row.new_lineno.or(row.old_lineno);
            lineno.is_none_or(|line| line < start || line > end)
        }
        _ => true,
    }
}

fn dim_spans(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    spans
        .into_iter()
        .map(|span| {
            let style = span.style.add_modifier(Modifier::DIM);
            Span::styled(span.content, style)
        })
        .collect()
}

/// Gutter + line number + prefix + text spans for one diff line, with the
/// visual cues applied (docs/focused-diff-ux.md precedence: cursor > range
/// selection > word emphasis > line background).
fn diff_line_cell_spans(
    session: &ReviewSession,
    row: &DiffRow,
    index: usize,
    lineno: Option<usize>,
    comment_count: usize,
) -> Vec<Span<'static>> {
    diff_line_cell_spans_with_width(session, row, index, lineno, comment_count, 4)
}

fn diff_line_cell_spans_with_width(
    session: &ReviewSession,
    row: &DiffRow,
    index: usize,
    lineno: Option<usize>,
    comment_count: usize,
    line_number_width: usize,
) -> Vec<Span<'static>> {
    let DiffRowKind::DiffLine(line_kind) = row.kind else {
        return vec![Span::raw(row.text.clone())];
    };
    let selected = session.focus == Focus::Diff && session.diff_cursor == index;
    let in_range = session.diff_row_in_active_range(index);
    let style = diff_row_style(row.kind, selected, in_range);
    let flagged = session.diff_row_flagged(row);
    let cues = &session.diff_cues;
    let (comment_mark, mark_style) = if comment_count > 0 {
        (
            match comment_count {
                1..=9 => comment_count.to_string(),
                _ => "+".to_owned(),
            },
            Style::default().fg(Color::Yellow),
        )
    } else if flagged {
        // Agent-flagged section: pinned in the gutter.
        (
            "!".to_owned(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    } else if in_range {
        ("|".to_owned(), Style::default().fg(Color::Yellow))
    } else if cues.gutter_bar && matches!(line_kind, DiffLineKind::Added | DiffLineKind::Removed) {
        // The bar fills otherwise-empty gutter cells on changed lines.
        match line_kind {
            DiffLineKind::Added => (
                "\u{258e}".to_owned(),
                syntax_style_spec(&cues.theme.gutter_added),
            ),
            _ => (
                "\u{258e}".to_owned(),
                syntax_style_spec(&cues.theme.gutter_removed),
            ),
        }
    } else {
        (" ".to_owned(), Style::default().fg(Color::Yellow))
    };

    // Cursor and range-selection backgrounds win over the cue backgrounds.
    let plain = !selected && !in_range;
    let line_bg = (plain && cues.line_background)
        .then(|| match line_kind {
            DiffLineKind::Added => spec_bg_color(&cues.theme.added_line_bg),
            DiffLineKind::Removed => spec_bg_color(&cues.theme.removed_line_bg),
            _ => None,
        })
        .flatten();
    let emphasis_style = (plain && cues.word_highlight && !row.emphasis.is_empty())
        .then(|| match line_kind {
            DiffLineKind::Added => Some(syntax_style_spec(&cues.theme.added_word)),
            DiffLineKind::Removed => Some(syntax_style_spec(&cues.theme.removed_word)),
            _ => None,
        })
        .flatten();
    let style = match line_bg {
        Some(bg) => style.bg(bg),
        None => style,
    };
    let lineno = lineno
        .map(|n| format!("{n:>line_number_width$}"))
        .unwrap_or_else(|| " ".repeat(line_number_width));
    let mut spans = vec![
        Span::styled(comment_mark, mark_style),
        Span::styled(lineno, Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(row.prefix, style),
        Span::styled(" ", style),
    ];
    spans.extend(diff_text_spans(
        row,
        style,
        selected,
        in_range,
        line_bg,
        emphasis_style,
        &session.syntax.theme,
    ));
    spans
}

/// Diff pane title: just "diff" normally; with the file pane hidden it
/// carries the selected file path and viewed mark so context is never lost.
fn diff_pane_title(session: &ReviewSession) -> String {
    if session.file_pane_visible {
        return "diff".to_owned();
    }
    match session.selected_visible_file() {
        Some(file) => format!(
            "diff · {}{}",
            file.path,
            if file.viewed {
                " ✓"
            } else if file.caught_up {
                " ◌"
            } else {
                ""
            }
        ),
        None => "diff".to_owned(),
    }
}

fn comment_summary_line(comment: &Comment) -> Line<'static> {
    let summary = comment
        .body
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("(empty comment)")
        .trim();
    // Comment ids are UUIDs; show a short prefix so the gutter stays readable.
    let short_id: String = comment.id.chars().take(8).collect();
    let mut spans = vec![
        Span::styled("      ↳ ", Style::default().fg(Color::Yellow)),
        Span::styled(format!("{short_id} "), Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("[{}] ", comment.state.label()),
            comment_state_style(comment.state),
        ),
    ];
    spans.extend(comment_badge_spans(comment));
    spans.push(Span::styled(
        summary.to_owned(),
        Style::default().fg(Color::Yellow),
    ));
    Line::from(spans)
}

fn comment_badge_spans(comment: &Comment) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if let Some(action) = comment.action
        && action != crate::state::ActionIntent::None
    {
        spans.push(Span::styled(
            format!("[{}] ", action_intent_label(action)),
            Style::default().fg(Color::Magenta),
        ));
    }
    if let Some(kind) = comment.kind {
        spans.push(Span::styled(
            format!("[{}] ", comment_kind_label(kind)),
            Style::default().fg(Color::Blue),
        ));
    }
    spans
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

fn comment_state_style(state: crate::state::CommentState) -> Style {
    match state {
        crate::state::CommentState::Draft => Style::default().fg(Color::DarkGray),
        crate::state::CommentState::Todo => Style::default().fg(Color::Red),
        crate::state::CommentState::Resolved => Style::default().fg(Color::Green),
    }
}

fn diff_text_spans(
    row: &DiffRow,
    fallback_style: Style,
    selected: bool,
    in_range: bool,
    line_bg: Option<Color>,
    emphasis_style: Option<Style>,
    theme: &SyntaxThemeConfig,
) -> Vec<Span<'static>> {
    let mut segments: Vec<(String, Style)> = if row.syntax.is_empty() {
        vec![(row.text.clone(), fallback_style)]
    } else {
        row.syntax
            .iter()
            .map(|span| {
                let mut style = syntax_span_style(span, selected, in_range, theme);
                if let Some(bg) = line_bg
                    && style.bg.is_none()
                {
                    style = style.bg(bg);
                }
                (span.text.clone(), style)
            })
            .collect()
    };
    if let Some(emphasis) = emphasis_style
        && !row.emphasis.is_empty()
    {
        segments = overlay_emphasis(segments, &row.emphasis, emphasis);
    }
    segments
        .into_iter()
        .map(|(text, style)| Span::styled(text, style))
        .collect()
}

/// Split styled segments at emphasis byte-range boundaries, patching the
/// emphasis style onto the covered slices. Ranges are byte offsets into the
/// concatenated segment text (the row text).
fn overlay_emphasis(
    segments: Vec<(String, Style)>,
    ranges: &[std::ops::Range<usize>],
    emphasis: Style,
) -> Vec<(String, Style)> {
    let mut result = Vec::new();
    let mut offset = 0usize;
    for (text, style) in segments {
        let end = offset + text.len();
        let mut cursor = 0usize;
        for range in ranges {
            let start = range.start.max(offset).min(end);
            let stop = range.end.max(offset).min(end);
            if start >= stop {
                continue;
            }
            let (local_start, local_stop) = (start - offset, stop - offset);
            if !text.is_char_boundary(local_start) || !text.is_char_boundary(local_stop) {
                continue;
            }
            if local_start > cursor {
                result.push((text[cursor..local_start].to_owned(), style));
            }
            result.push((
                text[local_start..local_stop].to_owned(),
                style.patch(emphasis),
            ));
            cursor = local_stop;
        }
        if cursor < text.len() {
            result.push((text[cursor..].to_owned(), style));
        }
        offset = end;
    }
    result
}

fn syntax_span_style(
    span: &SyntaxSpan,
    selected: bool,
    in_range: bool,
    theme: &SyntaxThemeConfig,
) -> Style {
    let style = match span.kind {
        Some(HighlightKind::Attribute) => syntax_style_spec(&theme.attribute),
        Some(HighlightKind::Comment) => syntax_style_spec(&theme.comment),
        Some(HighlightKind::Constant) => syntax_style_spec(&theme.constant),
        Some(HighlightKind::Function) => syntax_style_spec(&theme.function),
        Some(HighlightKind::Keyword) => syntax_style_spec(&theme.keyword),
        Some(HighlightKind::Number) => syntax_style_spec(&theme.number),
        Some(HighlightKind::Operator) => syntax_style_spec(&theme.operator),
        Some(HighlightKind::Property) => syntax_style_spec(&theme.property),
        Some(HighlightKind::Punctuation) => syntax_style_spec(&theme.punctuation),
        Some(HighlightKind::String) => syntax_style_spec(&theme.string),
        Some(HighlightKind::Type) => syntax_style_spec(&theme.r#type),
        Some(HighlightKind::Variable) | None => syntax_style_spec(&theme.variable),
    };

    if selected {
        style.bg(Color::DarkGray).add_modifier(Modifier::BOLD)
    } else if in_range {
        style.bg(Color::Blue)
    } else {
        style
    }
}

fn syntax_style_spec(spec: &str) -> Style {
    let mut style = Style::default();
    let mut background = false;
    for token in spec.split_whitespace() {
        if token == "on" {
            background = true;
            continue;
        }
        if let Some(color) = spec_color(token) {
            style = if background {
                style.bg(color)
            } else {
                style.fg(color)
            };
            background = false;
            continue;
        }
        style = match token {
            "bold" => style.add_modifier(Modifier::BOLD),
            "dim" => style.add_modifier(Modifier::DIM),
            "italic" => style.add_modifier(Modifier::ITALIC),
            "underlined" | "underline" => style.add_modifier(Modifier::UNDERLINED),
            _ => style,
        };
    }
    style
}

/// One color token: a named color, an indexed value (`22`), or `#rrggbb`.
fn spec_color(token: &str) -> Option<Color> {
    match token {
        "black" => Some(Color::Black),
        "blue" => Some(Color::Blue),
        "cyan" => Some(Color::Cyan),
        "dark-gray" | "dark-grey" => Some(Color::DarkGray),
        "gray" | "grey" => Some(Color::Gray),
        "green" => Some(Color::Green),
        "magenta" => Some(Color::Magenta),
        "red" => Some(Color::Red),
        "white" => Some(Color::White),
        "yellow" => Some(Color::Yellow),
        _ => {
            if let Some(hex) = token.strip_prefix('#') {
                if hex.len() != 6 {
                    return None;
                }
                let value = u32::from_str_radix(hex, 16).ok()?;
                return Some(Color::Rgb(
                    (value >> 16) as u8,
                    (value >> 8) as u8,
                    value as u8,
                ));
            }
            token.parse::<u8>().ok().map(Color::Indexed)
        }
    }
}

/// Background color for `*-line-bg` theme entries: the spec's background if
/// it uses `on <color>`, otherwise its first color read as a background.
fn spec_bg_color(spec: &str) -> Option<Color> {
    let style = syntax_style_spec(spec);
    style.bg.or(style.fg)
}

/// Whether the terminal advertises 24-bit color support.
pub(super) fn terminal_supports_truecolor() -> bool {
    std::env::var("COLORTERM")
        .map(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("truecolor") || value.contains("24bit")
        })
        .unwrap_or(false)
}

/// Rewrite `#rrggbb` tokens in the diff cue theme to their nearest
/// xterm-256 indexed colors, for terminals without truecolor support
/// (docs/focused-diff-ux.md §1). Named and indexed tokens pass through.
pub(super) fn downgrade_diff_theme(theme: &mut crate::config::DiffThemeConfig) {
    for spec in [
        &mut theme.added_line_bg,
        &mut theme.removed_line_bg,
        &mut theme.added_word,
        &mut theme.removed_word,
        &mut theme.gutter_added,
        &mut theme.gutter_removed,
    ] {
        *spec = quantize_spec(spec);
    }
}

fn quantize_spec(spec: &str) -> String {
    spec.split_whitespace()
        .map(|token| match token.strip_prefix('#') {
            Some(hex) if hex.len() == 6 => match u32::from_str_radix(hex, 16) {
                Ok(value) => nearest_indexed((value >> 16) as u8, (value >> 8) as u8, value as u8)
                    .to_string(),
                Err(_) => token.to_owned(),
            },
            _ => token.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Nearest xterm-256 color index for an RGB value, considering both the
/// 6x6x6 color cube (16..=231) and the grayscale ramp (232..=255).
fn nearest_indexed(red: u8, green: u8, blue: u8) -> u8 {
    fn cube_component(value: u8) -> (u8, u8) {
        // Cube levels: 0, 95, 135, 175, 215, 255.
        let levels = [0u8, 95, 135, 175, 215, 255];
        let mut best = (0u8, u16::MAX);
        for (index, level) in levels.into_iter().enumerate() {
            let distance = value.abs_diff(level) as u16;
            if distance < best.1 {
                best = (index as u8, distance);
            }
        }
        (best.0, [0u8, 95, 135, 175, 215, 255][best.0 as usize])
    }
    fn distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
        let dr = a.0.abs_diff(b.0) as u32;
        let dg = a.1.abs_diff(b.1) as u32;
        let db = a.2.abs_diff(b.2) as u32;
        dr * dr + dg * dg + db * db
    }

    let (ri, rv) = cube_component(red);
    let (gi, gv) = cube_component(green);
    let (bi, bv) = cube_component(blue);
    let cube_index = 16 + 36 * ri + 6 * gi + bi;

    // Only near-gray colors may land on the grayscale ramp: dark tints are
    // often numerically closer to a gray, but flattening the hue defeats
    // the point of a green/red cue.
    let spread = red.max(green).max(blue) - red.min(green).min(blue);
    if spread >= 12 {
        if cube_index == 16 {
            // A dark tint should stay a tint: bump the dominant channel to
            // the first cube level instead of flattening to black.
            let max = red.max(green).max(blue);
            return if red == max {
                16 + 36
            } else if green == max {
                16 + 6
            } else {
                16 + 1
            };
        }
        return cube_index;
    }

    let cube_distance = distance((red, green, blue), (rv, gv, bv));
    // Grayscale ramp: 8, 18, ..., 238.
    let gray = (red as u16 + green as u16 + blue as u16) / 3;
    let gray_step = ((gray.saturating_sub(8)).div_ceil(10)).min(23) as u8;
    let gray_value = 8 + 10 * gray_step;
    let gray_index = 232 + gray_step;
    let gray_distance = distance((red, green, blue), (gray_value, gray_value, gray_value));

    if gray_distance < cube_distance {
        gray_index
    } else {
        cube_index
    }
}

fn diff_row_style(kind: DiffRowKind, selected: bool, in_range: bool) -> Style {
    let style = match kind {
        DiffRowKind::DiffLine(DiffLineKind::Context) => Style::default().fg(Color::Gray),
        DiffRowKind::DiffLine(DiffLineKind::Added) => Style::default().fg(Color::Green),
        DiffRowKind::DiffLine(DiffLineKind::Removed) => Style::default().fg(Color::Red),
        DiffRowKind::DiffLine(DiffLineKind::Meta) => Style::default().fg(Color::DarkGray),
        _ => Style::default(),
    };

    if selected {
        style.bg(Color::DarkGray).add_modifier(Modifier::BOLD)
    } else if in_range {
        style.bg(Color::Blue)
    } else {
        style
    }
}

fn draw_footer(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    context: &FooterContext<'_>,
) {
    let mode = context.mode;
    let keymap = context.keymap;
    let zen = context.zen;
    let mode_text = match mode {
        Mode::Normal if zen.is_some() => {
            let zen = zen.expect("checked above");
            match zen.phase {
                ZenPhase::Focus => match zen.current() {
                    Some(ZenStop::Chapter(chapter)) => {
                        let mut text = format!(
                            "zen chapter {}/{} · {} tours its {} stop(s)",
                            chapter.position.0,
                            chapter.position.1,
                            keymap.hint(Action::ZenNext),
                            chapter.stop_count,
                        );
                        if !chapter.description_body().is_empty() {
                            let details = if zen.chapter_description_collapsed {
                                format!(" · {} details", keymap.hint(Action::ZenToggleDetails))
                            } else {
                                format!(
                                    " · {} collapse/expand brief",
                                    keymap.hint(Action::ZenToggleDetails)
                                )
                            };
                            text.push_str(&details);
                        }
                        if !chapter.artifacts.is_empty() {
                            text.push_str(&format!(
                                " · {} {} artifact(s)",
                                keymap.hint(Action::ZenArtifact),
                                chapter.artifacts.len()
                            ));
                        }
                        text.push_str(&format!(
                            " · {} back · {} full diff · {} glance · {} end",
                            keymap.hint(Action::ZenPrevious),
                            keymap.hint(Action::ZenToggleView),
                            keymap.hint(Action::ZenGlance),
                            keymap.hint(Action::PopupClose),
                        ));
                        text
                    }
                    current => {
                        let (current_stop, total) = zen.chunk_position();
                        let artifacts = current
                            .map(|stop| super::zen::stop_artifacts(stop).len())
                            .unwrap_or(0);
                        let artifact_hint = if artifacts > 0 {
                            format!(
                                " · {} {artifacts} artifact(s)",
                                keymap.hint(Action::ZenArtifact)
                            )
                        } else {
                            String::new()
                        };
                        format!(
                            "zen {current_stop}/{total} · {next} next (marks viewed) · {previous} back · {down}/{up} lines · {refocus} refocus · {view} full diff · {glance} glance{artifact_hint} · {comment} comment · {close} end",
                            next = keymap.hint(Action::ZenNext),
                            previous = keymap.hint(Action::ZenPrevious),
                            down = keymap.hint(Action::MoveDown),
                            up = keymap.hint(Action::MoveUp),
                            refocus = keymap.hint(Action::ZenRefocus),
                            view = keymap.hint(Action::ZenToggleView),
                            glance = keymap.hint(Action::ZenGlance),
                            comment = keymap.hint(Action::Comment),
                            close = keymap.hint(Action::PopupClose),
                        )
                    }
                },
                ZenPhase::Reading => {
                    let (current, total) = zen.chunk_position();
                    format!(
                        "zen read {current}/{total} · {} next · {} back · {} refocus · {} focus card · {} end · other keys as normal",
                        keymap.hint(Action::ZenNext),
                        keymap.hint(Action::ZenPrevious),
                        keymap.hint(Action::ZenRefocus),
                        keymap.hint(Action::ZenToggleView),
                        keymap.hint(Action::PopupClose),
                    )
                }
                ZenPhase::Glance => format!(
                    "zen glance · {} item(s) · {}/{} move · {} jump · {} mark all viewed & finish · {} back · {} end",
                    zen.glance_rows.len(),
                    keymap.hint(Action::PopupMoveDown),
                    keymap.hint(Action::PopupMoveUp),
                    keymap.hint(Action::PopupSelect),
                    keymap.hint(Action::ZenAcknowledge),
                    keymap.hint(Action::ZenPrevious),
                    keymap.hint(Action::PopupClose),
                ),
                ZenPhase::Artifact { index, .. } => {
                    let count = zen
                        .current()
                        .map(|stop| super::zen::stop_artifacts(stop).len())
                        .unwrap_or(0);
                    format!(
                        "zen artifact {}/{count} · {}/{} scroll · {}/{} switch · {}/{} close",
                        (index + 1).min(count),
                        keymap.hint(Action::PopupMoveDown),
                        keymap.hint(Action::PopupMoveUp),
                        keymap.hint(Action::ZenArtifactPrevious),
                        keymap.hint(Action::ZenArtifactNext),
                        keymap.hint(Action::PopupClose),
                        keymap.hint(Action::PopupCloseQ),
                    )
                }
            }
        }
        Mode::Normal if session.focus == Focus::Files => footer_line(files_footer_segments(
            session,
            zen,
            keymap,
            &format!(
                "focus files{}{}{}",
                if session.hide_generated {
                    " (noisy hidden)"
                } else {
                    ""
                },
                viewed_filter_label(session),
                if session.agent_order_active() {
                    " (agent order)"
                } else {
                    ""
                },
            ),
        )),
        Mode::Normal => {
            let mut text = footer_line(diff_footer_segments(
                session,
                zen,
                keymap,
                &format!(
                    "focus diff{}{}{}{}",
                    if session.has_active_diff_range() {
                        " (range active)"
                    } else {
                        ""
                    },
                    if session.hide_generated {
                        " (noisy hidden)"
                    } else {
                        ""
                    },
                    viewed_filter_label(session),
                    if session.diff_cues.soft_wrap {
                        " (wrap)"
                    } else {
                        " (nowrap)"
                    },
                ),
            ));
            if session.selected_comment().is_some() {
                text.push_str(&format!(
                    " · {} state · {} edit · {} delete",
                    keymap.hint(Action::CycleCommentState),
                    keymap.hint(Action::EditComment),
                    keymap.hint(Action::DeleteComment)
                ));
            }
            text
        }
        Mode::CommentInput { target, .. } => format!(
            "{kind} comment · refresh paused · {newline} newline · {submit} save · {cancel} cancel",
            kind = match target {
                CommentInputTarget::New => "new",
                CommentInputTarget::NewGeneral => "new general",
                CommentInputTarget::Edit { .. } => "edit",
                CommentInputTarget::AcceptDraft { .. } => "accept draft",
            },
            newline = keymap.hint(Action::InsertNewline),
            submit = keymap.hint(Action::SubmitComment),
            cancel = keymap.hint(Action::CancelComment),
        ),
        Mode::TargetChooser(_) => {
            format!(
                "choose base/tip · refresh paused · type filter · tab side · {down}/{up} move · {select} load · {close} cancel",
                down = keymap.hint(Action::TargetPickerMoveDown),
                up = keymap.hint(Action::TargetPickerMoveUp),
                select = keymap.hint(Action::PopupSelect),
                close = keymap.hint(Action::PopupClose),
            )
        }
        Mode::RevsetInput(_) => {
            format!(
                "revset target · refresh paused · type revset · tab/↑/↓ switch field · {} load · {} cancel",
                keymap.hint(Action::PopupSelect),
                keymap.hint(Action::PopupClose),
            )
        }
        Mode::OperationPicker(_) => {
            format!(
                "prior operation · refresh paused · {}/{} move · {} apply · {} cancel",
                keymap.hint(Action::PopupMoveDown),
                keymap.hint(Action::PopupMoveUp),
                keymap.hint(Action::PopupSelect),
                keymap.hint(Action::PopupClose),
            )
        }
        Mode::JjHelpers(state) => {
            if state.confirming {
                format!(
                    "confirm jj command · {} run · {} back",
                    keymap.hint(Action::PopupSelect),
                    keymap.hint(Action::PopupClose)
                )
            } else {
                list_popup_hint("jj helpers", keymap, "select")
            }
        }
        Mode::FlagList(_) => list_popup_hint("agent flags", keymap, "jump"),
        Mode::OpenWork(_) => list_popup_hint("action items & feedback", keymap, "jump"),
        Mode::Activity(_) => list_popup_hint("activity", keymap, "jump (file events)"),
        Mode::WalkthroughList(_) => {
            format!(
                "{} · {}/{} reorder · {} delete",
                list_popup_hint("walkthrough", keymap, "jump"),
                keymap.hint(Action::WalkthroughMoveDown),
                keymap.hint(Action::WalkthroughMoveUp),
                keymap.hint(Action::WalkthroughDelete),
            )
        }
        Mode::DraftList(_) => {
            format!(
                "agent drafts · {}/{} move · {} accept · {} edit · {} discard · {} close",
                keymap.hint(Action::PopupMoveDown),
                keymap.hint(Action::PopupMoveUp),
                keymap.hint(Action::DraftAccept),
                keymap.hint(Action::DraftEdit),
                keymap.hint(Action::DraftDiscard),
                keymap.hint(Action::PopupClose),
            )
        }
        Mode::FileSearch(_) => {
            format!(
                "file search · type filter · {down}/{up} move · {select} open · {close} cancel",
                down = keymap.hint(Action::TargetPickerMoveDown),
                up = keymap.hint(Action::TargetPickerMoveUp),
                select = keymap.hint(Action::PopupSelect),
                close = keymap.hint(Action::PopupClose),
            )
        }
        Mode::SymbolOutline(_) => list_popup_hint("changed symbols", keymap, "jump"),
        Mode::CommentList(_) => {
            format!(
                "comments · {down}/{up} move · {general} general · {select} jump · {edit} edit · {state} state · {ready} ready drafts · {action} action · {kind} kind · {delete} delete · {close} close",
                down = keymap.hint(Action::PopupMoveDown),
                up = keymap.hint(Action::PopupMoveUp),
                general = keymap.hint(Action::CommentListNewGeneral),
                select = keymap.hint(Action::PopupSelect),
                edit = keymap.hint(Action::EditComment),
                state = keymap.hint(Action::CycleCommentState),
                ready = keymap.hint(Action::CommentListReady),
                action = keymap.hint(Action::CommentListCycleIntent),
                kind = keymap.hint(Action::CommentListCycleKind),
                delete = keymap.hint(Action::DeleteComment),
                close = keymap.hint(Action::PopupClose),
            )
        }
        Mode::ViewOptions(_) => {
            format!(
                "view options · {}/{} move · {}/{} toggle · {}/{} close",
                keymap.hint(Action::PopupMoveDown),
                keymap.hint(Action::PopupMoveUp),
                keymap.hint(Action::PopupToggle),
                keymap.hint(Action::PopupSelect),
                keymap.hint(Action::PopupClose),
                keymap.hint(Action::PopupCloseQ),
            )
        }
        Mode::Help => format!(
            "help · {}/{} or page keys scroll · {}/{}/{} close",
            keymap.hint(Action::PopupMoveDown),
            keymap.hint(Action::PopupMoveUp),
            keymap.hint(Action::PopupClose),
            keymap.hint(Action::PopupCloseQ),
            keymap.hint(Action::Help),
        ),
    };
    let mut summary = session.summary_line();
    let display_target = context
        .zen
        .map(|zen| &zen.home_target)
        .unwrap_or(&session.target);
    if display_target.is_symbolic() {
        summary.push_str(&format!(" · following {}", display_target.rev));
    }
    if let Some(identity) = context.identity_chip {
        let prefix_width = summary.width() + " · ".width();
        let chip_width = area.width as usize;
        let chip_width = chip_width.saturating_sub(prefix_width);
        if chip_width > 0 {
            summary.push_str(" · ");
            summary.push_str(&truncate_tail(identity, chip_width));
        }
    }
    let mut lines = vec![Line::from(summary), Line::from(mode_text)];
    if let Some(notice) = context.notice {
        let (label, style) = match notice.level {
            UiNoticeLevel::Info => ("info", Style::default().fg(Color::Blue)),
            UiNoticeLevel::Error => ("error", Style::default().fg(Color::Red)),
        };
        let message_width = area.width.saturating_sub((label.len() + 2) as u16);
        lines[1] = Line::from(vec![
            Span::styled(format!("{label}: "), style.add_modifier(Modifier::BOLD)),
            Span::styled(
                notice_message_for_width(&notice.message, message_width),
                style,
            ),
        ]);
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

/// One footer entry: the keys that trigger it plus a short label.
struct FooterHint {
    keys: Vec<Action>,
    label: String,
}

impl FooterHint {
    fn new(keys: impl Into<Vec<Action>>, label: impl Into<String>) -> Self {
        Self {
            keys: keys.into(),
            label: label.into(),
        }
    }

    fn render(&self, keymap: &KeyMap) -> String {
        let keys: Vec<&str> = self
            .keys
            .iter()
            .map(|action| keymap.hint(*action))
            .collect();
        format!("{} {}", keys.join("/"), self.label)
    }
}

fn footer_line(segments: Vec<String>) -> String {
    segments.join(" · ")
}

fn list_popup_hint(title: &str, keymap: &KeyMap, select_label: &str) -> String {
    format!(
        "{title} · {}/{} move · {} {select_label} · {} close",
        keymap.hint(Action::PopupMoveDown),
        keymap.hint(Action::PopupMoveUp),
        keymap.hint(Action::PopupSelect),
        keymap.hint(Action::PopupClose),
    )
}

fn hint_segments(keymap: &KeyMap, hints: &[FooterHint]) -> Vec<String> {
    hints.iter().map(|hint| hint.render(keymap)).collect()
}

fn files_footer_segments(
    session: &ReviewSession,
    zen: Option<&ZenState>,
    keymap: &KeyMap,
    focus_label: &str,
) -> Vec<String> {
    // Deliberately short: the everyday loop only. Everything else lives in
    // the help overlay so the footer stays readable at a glance.
    let hints = [
        FooterHint::new([Action::MoveDown, Action::MoveUp], "move"),
        FooterHint::new([Action::ToggleFold], "fold"),
        FooterHint::new([Action::CycleViewedFilter], "filter"),
        FooterHint::new([Action::FileSearch], "search"),
        FooterHint::new([Action::NextUnviewed, Action::PreviousUnviewed], "unviewed"),
        FooterHint::new([Action::MarkViewed], "viewed"),
        FooterHint::new([Action::ToggleFocus], "diff"),
        FooterHint::new([Action::Comment], "comment"),
        FooterHint::new([Action::YankHandoff], "handoff"),
        FooterHint::new([Action::Help], "help"),
        FooterHint::new([Action::Quit], "quit"),
    ];
    let target = zen
        .map(|zen| zen.home_target.to_string())
        .unwrap_or_else(|| session.target.to_string());
    let mut segments = vec![target, focus_label.to_owned()];
    segments.extend(hint_segments(keymap, &hints));
    segments
}

fn diff_footer_segments(
    session: &ReviewSession,
    zen: Option<&ZenState>,
    keymap: &KeyMap,
    focus_label: &str,
) -> Vec<String> {
    let hints = [
        FooterHint::new([Action::MoveDown, Action::MoveUp], "line"),
        FooterHint::new([Action::ScrollDown, Action::ScrollUp], "scroll"),
        FooterHint::new([Action::ViewOptions], "view/wrap"),
        FooterHint::new([Action::RangeComment], "range"),
        FooterHint::new([Action::MarkWalkthrough], "walkthrough"),
        FooterHint::new([Action::Comment], "comment"),
        FooterHint::new([Action::YankHandoff], "handoff"),
        FooterHint::new([Action::NextUnviewed, Action::PreviousUnviewed], "unviewed"),
        FooterHint::new([Action::ToggleFocus], "files"),
        FooterHint::new([Action::Help], "help"),
        FooterHint::new([Action::Quit], "quit"),
    ];
    let target = zen
        .map(|zen| zen.home_target.to_string())
        .unwrap_or_else(|| session.target.to_string());
    let mut segments = vec![target, focus_label.to_owned()];
    segments.extend(hint_segments(keymap, &hints));
    segments
}

fn viewed_filter_label(session: &ReviewSession) -> String {
    session
        .viewed_filter
        .label()
        .map(|label| format!(" ({label})"))
        .unwrap_or_default()
}

/// Full keymap reference, grouped by workflow. The footer only shows the
/// everyday hints; this popup is the complete map. Two independently wrapped
/// columns share a scroll offset so every group remains reachable.
fn draw_help_popup(frame: &mut ratatui::Frame<'_>, area: Rect, keymap: &KeyMap, scroll: usize) {
    let popup = centered_rect(90, 80, area);
    frame.render_widget(Clear, popup);

    let section = |title: &str| {
        Line::from(Span::styled(
            title.to_owned(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let entry = |actions: &[Action], label: &str| {
        let keys: Vec<&str> = actions.iter().map(|action| keymap.hint(*action)).collect();
        Line::from(vec![
            Span::styled(
                format!("  {:>10}  ", keys.join("/")),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(label.to_owned(), Style::default().fg(Color::Gray)),
        ])
    };
    let literal = |keys: &str, label: &str| {
        Line::from(vec![
            Span::styled(
                format!("  {:>10}  ", keys),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(label.to_owned(), Style::default().fg(Color::Gray)),
        ])
    };

    let left = vec![
        section("first review loop"),
        entry(&[Action::FileSearch], "open a file or fuzzy jump"),
        entry(&[Action::NextUnviewed], "next unviewed file"),
        entry(&[Action::MarkViewed], "mark viewed and advance"),
        entry(&[Action::Comment], "comment on what needs work"),
        entry(&[Action::Zen], "zen briefing for a focused pass"),
        entry(&[Action::YankHandoff], "copy handoff when done"),
        section("diff & view"),
        entry(&[Action::MoveDown, Action::MoveUp], "move diff cursor"),
        entry(&[Action::ScrollDown, Action::ScrollUp], "vertical scroll"),
        entry(
            &[Action::ScrollDiffLeft, Action::ScrollDiffRight],
            "horizontal scroll when wrap is off",
        ),
        entry(&[Action::ViewOptions], "soft wrap, layout, and visual cues"),
        section("general"),
        entry(
            &[Action::ToggleFocus],
            "switch focus between files and diff",
        ),
        entry(&[Action::ToggleFilePane], "hide/show file pane"),
        entry(&[Action::Help], "this help"),
        entry(
            &[Action::Activity],
            "activity feed (live refresh while open)",
        ),
        entry(&[Action::CancelRangeComment], "dismiss range/notice"),
        entry(&[Action::Quit], "quit (writes artifact)"),
        section("files"),
        entry(&[Action::MoveDown, Action::MoveUp], "move in tree"),
        entry(&[Action::ToggleFold], "fold/unfold directory"),
        entry(
            &[Action::CollapseFold, Action::ExpandFold],
            "collapse/expand directory",
        ),
        entry(&[Action::FileSearch], "fuzzy file search"),
        entry(&[Action::ToggleGenerated], "hide/show noisy files"),
        entry(&[Action::CycleViewedFilter], "cycle viewed filter"),
        entry(
            &[Action::NextUnviewed, Action::PreviousUnviewed],
            "next/previous unviewed file",
        ),
        entry(&[Action::MarkViewed], "mark viewed and advance"),
        entry(&[Action::ToggleViewed], "toggle viewed"),
        entry(&[Action::MarkAllViewed], "mark all viewed"),
        section("diff"),
        entry(&[Action::MoveDown, Action::MoveUp], "move cursor"),
        entry(&[Action::ScrollDown, Action::ScrollUp], "scroll"),
        entry(
            &[Action::ScrollDiffLeft, Action::ScrollDiffRight],
            "horizontal scroll (wrapping off)",
        ),
        entry(&[Action::DiffTop, Action::DiffBottom], "jump top/bottom"),
        entry(
            &[Action::NextSymbol, Action::PreviousSymbol],
            "next/previous changed symbol",
        ),
        entry(
            &[Action::NextChangedHunk, Action::PreviousChangedHunk],
            "next/previous changed hunk",
        ),
        entry(&[Action::SymbolOutline], "changed symbol outline"),
        entry(&[Action::ToggleContextFold], "fold/unfold context lines"),
        entry(
            &[
                Action::ExpandContext,
                Action::ExpandContextAll,
                Action::CollapseContext,
            ],
            "expand/collapse hidden context",
        ),
        entry(&[Action::ViewOptions], "view options (wrap/layout/cues)"),
        entry(&[Action::ToggleDiffView], "toggle side-by-side view"),
        entry(&[Action::ToggleLargeDiff], "expand/collapse huge diff"),
    ];
    let right = vec![
        section("zen keys"),
        entry(&[Action::ZenNext, Action::ZenPrevious], "next/back stop"),
        entry(&[Action::ZenToggleView], "toggle focus card / reading view"),
        entry(&[Action::ZenGlance], "open glance board"),
        entry(&[Action::ZenArtifact], "open/close artifacts"),
        entry(
            &[Action::ZenToggleDetails],
            "toggle chapter details / expand brief",
        ),
        entry(&[Action::ZenRefocus], "refocus current stop"),
        entry(&[Action::ZenAcknowledge], "acknowledge glance items"),
        entry(&[Action::PopupClose], "leave zen / close artifact"),
        section("comments"),
        entry(&[Action::Comment], "comment at cursor"),
        entry(&[Action::RangeComment], "start/finish range comment"),
        entry(&[Action::CycleCommentState], "cycle comment state"),
        entry(&[Action::EditComment], "edit comment"),
        entry(&[Action::DeleteComment], "delete comment"),
        entry(&[Action::CommentList], "comment center"),
        entry(
            &[Action::CommentList, Action::CommentListNewGeneral],
            "create general comment (sequence)",
        ),
        entry(&[Action::CommentListReady], "ready all draft comments"),
        entry(&[Action::OpenWork], "action items & todo feedback"),
        entry(&[Action::WalkthroughList], "walkthrough panel"),
        entry(&[Action::MarkWalkthrough], "mark for walkthrough"),
        entry(
            &[Action::NextComment, Action::PreviousComment],
            "next/previous comment",
        ),
        section("targets & jj"),
        entry(&[Action::CompareTrunk], "return to launch target"),
        entry(&[Action::CompareParent], "compare @-..@"),
        entry(&[Action::TargetChooser], "base/tip chooser"),
        entry(&[Action::RevsetInput], "revset input"),
        entry(
            &[Action::StackNext, Action::StackPrevious],
            "step through stack",
        ),
        entry(
            &[Action::OperationPicker],
            "diff against prior jj operation",
        ),
        entry(&[Action::JjHelpers], "jj helpers (squash, rebase, ...)"),
        section("agent"),
        entry(&[Action::SummonAgent], "summon configured review agent"),
        entry(&[Action::YankHandoff], "copy agent handoff markdown"),
        entry(&[Action::ToggleAgentOrder], "toggle agent-suggested order"),
        entry(&[Action::FlagList], "agent-flagged sections"),
        entry(&[Action::Zen], "zen briefing (focus stops + glance)"),
        entry(&[Action::DraftList], "agent draft comments"),
        section("badges"),
        literal(
            "✓",
            "• unviewed · ✓ viewed · ◌ caught up · ~ viewed, changed since · ± unviewed, changed since",
        ),
    ];

    frame.render_widget(Block::default().borders(Borders::ALL).title("help"), popup);
    let inner = inner_bordered(popup);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner);
    let scroll = scroll.min(u16::MAX as usize) as u16;
    frame.render_widget(
        Paragraph::new(left)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(right)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        columns[1],
    );
}

fn draw_comment_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    editor: &CommentEditor,
    target: &CommentInputTarget,
    keymap: &KeyMap,
) {
    let popup = comment_popup_rect(area);
    let inner = inner_bordered(popup);
    let layout = editor.layout(inner.width as usize);
    let scroll = editor.visible_scroll(inner.width as usize, inner.height as usize);
    let rows = layout
        .rows()
        .iter()
        .enumerate()
        .skip(scroll)
        .take(inner.height as usize)
        .map(|(row, _)| Line::raw(layout.row_text(row)))
        .collect::<Vec<_>>();
    frame.render_widget(Clear, popup);
    let title = comment_popup_title(session, target, popup.width.saturating_sub(4) as usize);
    let hint = format!(
        "{} save · {} cancel",
        keymap.hint(Action::SubmitComment),
        keymap.hint(Action::CancelComment)
    );
    frame.render_widget(
        Paragraph::new(rows).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .title_bottom(Line::from(Span::styled(
                    hint,
                    Style::default().fg(Color::DarkGray),
                ))),
        ),
        popup,
    );

    let cursor = layout.cursor_position(editor.cursor);
    frame.set_cursor_position((
        inner.x.saturating_add(cursor.column as u16),
        inner
            .y
            .saturating_add(cursor.row.saturating_sub(scroll) as u16),
    ));
}

pub(super) fn comment_editor_inner(area: Rect) -> Rect {
    inner_bordered(comment_popup_rect(area))
}

fn comment_popup_rect(area: Rect) -> Rect {
    centered_rect(70, 40, area)
}

fn comment_popup_title(
    session: &ReviewSession,
    target: &CommentInputTarget,
    max_width: usize,
) -> String {
    let kind = match target {
        CommentInputTarget::New => "comment",
        CommentInputTarget::NewGeneral => "general comment",
        CommentInputTarget::Edit { .. } => "edit comment",
        CommentInputTarget::AcceptDraft { .. } => "accept draft",
    };
    let target_anchor = match target {
        CommentInputTarget::Edit { id } => session
            .comments
            .iter()
            .find(|comment| &comment.id == id)
            .and_then(|comment| comment.anchor.clone()),
        _ => session.selected_comment_anchor(),
    };
    let is_general = matches!(target, CommentInputTarget::NewGeneral)
        || matches!(target, CommentInputTarget::Edit { id } if session.comments.iter().any(|comment| &comment.id == id && comment.is_general()));
    let location = if is_general {
        "general".to_owned()
    } else {
        target_anchor
            .map(
                |anchor| match (anchor.path(), anchor.line(), anchor.end_line()) {
                    (path, Some(line), Some(end)) if end != line => format!("{path}:{line}-{end}"),
                    (path, Some(line), _) => format!("{path}:{line}"),
                    (path, None, _) => path.to_owned(),
                },
            )
            .unwrap_or_else(|| "unanchored".to_owned())
    };
    truncate_middle(&format!("{kind} · {location}"), max_width)
}

fn draw_revset_input_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    input: &RevsetInputState,
    keymap: &KeyMap,
) {
    let popup = centered_rect(70, 30, area);
    frame.render_widget(Clear, popup);

    let field_line = |label: &str, value: &str, active: bool| {
        let marker = if active { "›" } else { " " };
        let value_style = if active {
            Style::default().fg(Color::White)
        } else {
            Style::default().fg(Color::Gray)
        };
        Line::from(vec![
            Span::styled(
                format!("{marker} {label:>4}: "),
                if active {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
            Span::styled(value.to_owned(), value_style),
        ])
    };

    let lines = vec![
        Line::from(Span::styled(
            "Review an arbitrary revset range (jj revset syntax)",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        field_line("base", &input.base, input.editing == RevsetField::Base),
        field_line("tip", &input.tip, input.editing == RevsetField::Tip),
        Line::from(""),
        Line::from(Span::styled(
            format!(
                "type revset · tab/↑/↓ switch field · {} load · {} cancel",
                keymap.hint(Action::PopupSelect),
                keymap.hint(Action::PopupClose)
            ),
            Style::default().fg(Color::DarkGray),
        )),
    ];

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("revsets"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn draw_jj_helpers_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    state: &JjHelperState,
    keymap: &KeyMap,
) {
    let popup = centered_rect(76, 46, area);
    frame.render_widget(Clear, popup);

    let mut lines = Vec::new();
    if state.confirming {
        let command = state
            .selected_option()
            .map(JjHelperOption::command_line)
            .unwrap_or_default();
        lines.push(Line::from(Span::styled(
            "About to run:",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {command}"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "This rewrites history in your repo. {} run · {} back",
                keymap.hint(Action::PopupSelect),
                keymap.hint(Action::PopupClose)
            ),
            Style::default().fg(Color::Red),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "jj helpers (nothing runs until you confirm)",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
        for (index, option) in state.options.iter().enumerate() {
            let selected = index == state.selected;
            let marker = if selected { "›" } else { " " };
            let style = if selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{marker} {}", option.label), style),
                Span::styled(
                    format!("  ({})", option.command_line()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            list_popup_hint("", keymap, "select")
                .trim_start_matches(" · ")
                .to_owned(),
            Style::default().fg(Color::DarkGray),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("jj helpers"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn draw_view_options_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    state: &ViewOptionsState,
    keymap: &KeyMap,
) {
    let popup = centered_rect(56, 70, area);
    frame.render_widget(Clear, popup);

    let mut lines = vec![Line::from(Span::styled(
        "Diff visual cues (session only; gander.toml [diff] sets defaults)",
        Style::default().fg(Color::DarkGray),
    ))];
    lines.push(Line::from(""));
    for (index, option) in ViewOption::ALL.into_iter().enumerate() {
        let selected = index == state.selected;
        let marker = if selected { "›" } else { " " };
        let checkbox = if option.enabled(session) {
            "[x]"
        } else {
            "[ ]"
        };
        let style = if selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker} "), style),
            Span::styled(format!("{checkbox} "), style),
            Span::styled(option.label().to_owned(), style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "{}/{} move · {}/{} toggle · {}/{} close",
            keymap.hint(Action::PopupMoveDown),
            keymap.hint(Action::PopupMoveUp),
            keymap.hint(Action::PopupToggle),
            keymap.hint(Action::PopupSelect),
            keymap.hint(Action::PopupClose),
            keymap.hint(Action::PopupCloseQ),
        ),
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("view options"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn draw_flag_list_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    list: &FlagListState,
    keymap: &KeyMap,
) {
    let popup = centered_rect(80, 60, area);
    frame.render_widget(Clear, popup);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 4usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window = picker_visible_window(list.selected, list.flags.len(), list_height);

    let mut lines = vec![Line::from(Span::styled(
        "Sections flagged by agents, critical first",
        Style::default().fg(Color::DarkGray),
    ))];
    if list.flags.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no flags",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines.extend(
            list.flags
                .iter()
                .enumerate()
                .skip(visible_window.start)
                .take(visible_window.end.saturating_sub(visible_window.start))
                .map(|(index, flag)| {
                    let selected = index == list.selected;
                    let marker = if selected { "›" } else { " " };
                    let style = if selected {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    let location = match flag.line {
                        Some(line) => format!("{}:{line}", flag.path),
                        None => flag.path.clone(),
                    };
                    Line::from(vec![
                        Span::styled(format!("{marker} "), style),
                        Span::styled(
                            format!("[{:^8}] ", flag.priority.label()),
                            flag_priority_style(flag.priority),
                        ),
                        Span::styled(format!("{location} "), Style::default().fg(Color::Cyan)),
                        Span::styled(flag.reason.clone(), style),
                    ])
                }),
        );
        if visible_window.hidden_below > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more", visible_window.hidden_below),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        list_popup_hint("", keymap, "jump")
            .trim_start_matches(" · ")
            .to_owned(),
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("agent flags"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

/// A compact bottom-anchored panel (not a modal popup): the diff pane stays
/// visible and follows the current stop while the panel shows progress,
/// location, the agent's rationale, and flags in the stop's file — the
/// orientation device that replaces the hidden file tree
/// (docs/focused-diff-ux.md §6).
fn draw_zen_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    zen: &ZenState,
    keymap: &KeyMap,
) {
    let height = if zen.glance_rows.is_empty() { 6 } else { 8 }.min(area.height);
    let panel = Rect {
        x: area.x,
        y: area
            .y
            .saturating_add(area.height.saturating_sub(height + 2)),
        width: area.width,
        height,
    };
    frame.render_widget(Clear, panel);

    let mut lines = Vec::new();
    let mut flagged = 0usize;
    match zen.current() {
        Some(ZenStop::Chapter(chapter)) => {
            let (number, total) = chapter.position;
            let headline = if chapter.description.is_empty() {
                zen.home_target.to_string()
            } else {
                chapter.title().to_owned()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("chapter {number}/{total}  "),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate_tail(&headline, area.width.saturating_sub(18) as usize),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::from(Span::styled(
                chapter.summary.clone().unwrap_or_else(|| {
                    "(no change brief from the agent — @ summons one)".to_owned()
                }),
                Style::default().fg(Color::Gray),
            )));
        }
        Some(ZenStop::Chunk(stop)) => {
            let position = stop
                .part_position
                .map(|(part, total)| format!(" (part {part}/{total})"))
                .unwrap_or_default();
            let location = chunk_row_location_width(stop, area.width.saturating_div(2) as usize);
            flagged = stop
                .part
                .as_ref()
                .map(|part| {
                    session
                        .agent_flags
                        .iter()
                        .filter(|flag| flag.path == part.path)
                        .count()
                })
                .unwrap_or(0);
            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        "{}{}  ",
                        truncate_tail(&stop.title, area.width.saturating_div(2) as usize),
                        position
                    ),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(location, Style::default().fg(Color::Cyan)),
            ]));
            lines.push(Line::from(Span::styled(
                stop.rationale
                    .clone()
                    .unwrap_or_else(|| "(no rationale given)".to_owned()),
                Style::default().fg(Color::Gray),
            )));
        }
        None => {}
    }
    if !zen.glance_rows.is_empty() {
        let shown = zen.glance_rows.iter().take(3).map(|row| {
            let location = chunk_row_location_width(row, 32);
            format!("{} @ {location}", truncate_tail(&row.title, 24))
        });
        let mut glance = shown.collect::<Vec<_>>().join("  ·  ");
        let hidden = zen.glance_rows.len().saturating_sub(3);
        if hidden > 0 {
            glance.push_str(&format!("  ·  +{hidden} more"));
        }
        lines.push(Line::from(vec![
            Span::styled(
                "glance rail  ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(glance, Style::default().fg(Color::DarkGray)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "{} next (marks viewed) · {} back · {} refocus · {} focus card · {} end · comment/flag/expand as normal",
            keymap.hint(Action::ZenNext),
            keymap.hint(Action::ZenPrevious),
            keymap.hint(Action::ZenRefocus),
            keymap.hint(Action::ZenToggleView),
            keymap.hint(Action::PopupClose),
        ),
        Style::default().fg(Color::DarkGray),
    )));

    let mut title = match zen.current() {
        Some(ZenStop::Chapter(chapter)) => {
            format!("zen chapter {}/{}", chapter.position.0, chapter.position.1)
        }
        _ => {
            let (current, total) = zen.chunk_position();
            format!("zen spotlight {current}/{total}")
        }
    };
    if !zen.glance_rows.is_empty() {
        title.push_str(&format!(" · {} glance", zen.glance_rows.len()));
    }
    if flagged > 0 {
        title.push_str(&format!(" · {flagged} flagged"));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(truncate_tail(
                &title,
                panel.width.saturating_sub(4) as usize,
            )))
            .wrap(Wrap { trim: false }),
        panel,
    );
}

fn chunk_row_location_width(row: &super::chunks::WalkthroughRow, max_width: usize) -> String {
    match (&row.change_id, &row.part) {
        (Some(change_id), Some(part)) => {
            let location = match (part.start_line, part.end_line) {
                (Some(start), Some(end)) => format!("{}:{start}-{end}", part.path),
                (Some(start), None) => format!("{}:{start}", part.path),
                _ => part.path.clone(),
            };
            let id_budget = (max_width / 4).clamp(4, 12);
            let path_budget = max_width.saturating_sub(id_budget + 3);
            format!(
                "[{}] {}",
                truncate_middle(change_id, id_budget),
                truncate_middle(&location, path_budget)
            )
        }
        _ => truncate_middle(&chunk_row_location_raw(row), max_width),
    }
}

fn chunk_row_location_raw(row: &super::chunks::WalkthroughRow) -> String {
    let location = match &row.part {
        Some(part) => match (part.start_line, part.end_line) {
            (Some(start), Some(end)) => format!("{}:{start}-{end}", part.path),
            (Some(start), None) => format!("{}:{start}", part.path),
            _ => part.path.clone(),
        },
        None => "(no location)".to_owned(),
    };
    // Change-anchored chunks tour their own change's diff: say which one.
    match &row.change_id {
        Some(change_id) => format!("[{change_id}] {location}"),
        None => location,
    }
}

fn truncate_tail(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width <= 1 {
        return "…".to_owned();
    }
    let mut out = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if width + w > max_width - 1 {
            break;
        }
        out.push(ch);
        width += w;
    }
    out.push('…');
    out
}

fn truncate_middle(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width <= 1 {
        return "…".to_owned();
    }
    let left_w = (max_width - 1) / 3;
    let right_w = max_width - 1 - left_w;
    let left = take_width_prefix(text, left_w);
    let right = take_width_suffix(text, right_w);
    format!("{left}…{right}")
}

fn take_width_prefix(text: &str, max_width: usize) -> String {
    let mut out = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if width + w > max_width {
            break;
        }
        out.push(ch);
        width += w;
    }
    out
}

fn take_width_suffix(text: &str, max_width: usize) -> String {
    let mut chars = Vec::new();
    let mut width = 0;
    for ch in text.chars().rev() {
        let w = ch.width().unwrap_or(0);
        if width + w > max_width {
            break;
        }
        chars.push(ch);
        width += w;
    }
    chars.into_iter().rev().collect()
}

/// The zen focus surface: a chapter intro card between changes, or a
/// spotlight stop card inside one (docs/focused-diff-ux.md §6).
fn draw_zen_focus(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    zen: &ZenState,
    keymap: &KeyMap,
) {
    match zen.current() {
        Some(ZenStop::Chapter(chapter)) => {
            draw_zen_chapter(frame, area, session, zen, chapter, keymap)
        }
        Some(ZenStop::Chunk(stop)) => draw_zen_stop(frame, area, session, zen, stop, keymap),
        None => {}
    }
}

/// The chapter card: a full-screen intro for one jj change before its
/// stops — description, bookmarks, live diff stats, and the agent's
/// high-level brief. Kills the "dropped into a random change id" feeling
/// of stacked walkthroughs: every change opens with its story.
fn draw_zen_chapter(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    zen: &ZenState,
    chapter: &super::zen::ChapterCard,
    keymap: &KeyMap,
) {
    frame.render_widget(Clear, area);
    let inner = slide_inner(area);
    let (number, total) = chapter.position;

    // Where this chapter lives: its own change, or the walkthrough's target.
    let mut location = match &chapter.change_id {
        Some(change_id) => format!("change {change_id}"),
        None => zen.home_target.to_string(),
    };
    if !chapter.bookmarks.is_empty() {
        location.push_str(&format!(" · {}", chapter.bookmarks));
    }

    let headline = if chapter.title().is_empty() {
        "chapter".to_owned()
    } else {
        chapter.title().to_owned()
    };
    let description_body = chapter.description_body();
    let additions: usize = session.files.iter().map(|file| file.additions).sum();
    let deletions: usize = session.files.iter().map(|file| file.deletions).sum();
    let stats = format!(
        "{} file(s) · +{additions} −{deletions}",
        session.files.len()
    );
    let mut stops_hint = if chapter.stop_count == 0 {
        "0 stops — skim".to_owned()
    } else {
        format!("{} stop(s) in this chapter", chapter.stop_count)
    };
    if !chapter.artifacts.is_empty() {
        stops_hint.push_str(&format!(
            " · e opens {} artifact(s)",
            chapter.artifacts.len()
        ));
    }

    let mut body: Vec<Line<'static>> = vec![
        Line::from(vec![
            Span::styled(
                format!("chapter {number}/{total} · {location}"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(
                zen_progress_label(zen),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            headline,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    if !chapter.subtitle().is_empty() {
        body.push(Line::from(Span::styled(
            chapter.subtitle().to_owned(),
            Style::default().fg(Color::DarkGray),
        )));
    }
    // The change's own words come right after the headline: the full
    // description body, collapsible with `d` when it gets in the way.
    if !description_body.is_empty() {
        if zen.chapter_description_collapsed {
            body.push(Line::from(Span::styled(
                format!(
                    "… d expands the description ({} more line(s))",
                    description_body.len()
                ),
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            body.push(Line::from(""));
            push_text_lines(
                &mut body,
                &description_body.join("\n"),
                Style::default().fg(Color::Gray),
            );
        }
    }
    body.push(Line::from(""));
    body.push(Line::from(Span::styled(
        stats,
        Style::default().fg(Color::Cyan),
    )));
    body.push(Line::from(Span::styled(
        stops_hint,
        Style::default().fg(Color::DarkGray),
    )));
    let has_curated_brief = chapter.summary.is_some();
    let summary = chapter.summary.clone().unwrap_or_else(|| {
        "No agent brief for this change — the facts above are derived from the diff.".to_owned()
    });
    if has_curated_brief {
        body.push(Line::from(""));
        body.push(Line::from(Span::styled(
            "what this change does",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )));
        for line in summary.lines() {
            body.push(Line::from(Span::styled(
                line.to_owned(),
                Style::default().fg(Color::Gray),
            )));
        }
    } else if !chapter.derived_lines.is_empty() {
        body.push(Line::from(""));
        body.push(Line::from(Span::styled(
            "derived facts",
            Style::default().fg(Color::DarkGray),
        )));
        for line in chapter.derived_lines.iter().take(3) {
            body.push(Line::from(Span::styled(
                truncate_tail(&format!("  {line}"), inner.width as usize),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    if !chapter.derived_lines.is_empty() && inner.height < 18 {
        body.push(Line::from(Span::styled(
            format!("derived: {}", derived_summary(&chapter.derived_lines)),
            Style::default().fg(Color::DarkGray),
        )));
    }
    if chapter.stop_count > 0 {
        body.push(Line::from(""));
        body.push(Line::from(Span::styled(
            format!(
                "{} to begin — {} stops",
                keymap.hint(Action::ZenNext),
                chapter.stop_count
            ),
            Style::default().fg(Color::Cyan),
        )));
    }
    if inner.height < 8 {
        frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), inner);
        return;
    }
    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), inner);
}

fn slide_inner(area: Rect) -> Rect {
    slide_column(area, 72)
}

fn slide_column(area: Rect, target_width: usize) -> Rect {
    let available = area.width as usize;
    let width = target_width
        .clamp(20.min(available), available)
        .min(available) as u16;
    // Keep the deck from drifting to the exact center on wide terminals. A
    // one-third leftover margin feels editorial: enough air at the right edge,
    // with the reading column anchored in a stable, slightly-left-of-center spot.
    let margin = area.width.saturating_sub(width) / 3;
    Rect {
        x: area.x + margin,
        y: area.y + 1.min(area.height),
        width,
        height: area.height.saturating_sub(2),
    }
}

fn measured_content_width(available: u16, excerpt_width: usize) -> usize {
    let available = available as usize;
    if available < 72 {
        return available;
    }
    excerpt_width.clamp(72, 100).min(available)
}

fn line_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|span| span.content.width()).sum()
}

fn wrapped_line_height(line: &Line<'_>, width: usize) -> usize {
    let line_width = line_width(line);
    if line_width == 0 || width == 0 {
        1
    } else {
        line_width.div_ceil(width).max(1)
    }
}

fn zen_progress_label(zen: &ZenState) -> String {
    let (chapter, chapters) = current_chapter_position(zen);
    format!("ch {chapter}/{chapters}")
}

fn current_chapter_position(zen: &ZenState) -> (usize, usize) {
    let total = zen
        .stops
        .iter()
        .filter(|stop| matches!(stop, ZenStop::Chapter(_)))
        .count()
        .max(1);
    let current = zen
        .stops
        .iter()
        .take(zen.index.saturating_add(1))
        .filter(|stop| matches!(stop, ZenStop::Chapter(_)))
        .count()
        .max(1);
    (current, total)
}

fn push_text_lines(lines: &mut Vec<Line<'static>>, text: &str, style: Style) {
    for raw in text.split('\n') {
        if raw.trim().is_empty() {
            lines.push(Line::from(""));
        } else {
            lines.push(Line::from(Span::styled(raw.to_owned(), style)));
        }
    }
}

fn derived_summary(lines: &[String]) -> String {
    let churn = lines
        .iter()
        .find_map(|line| line.strip_prefix("churn: "))
        .unwrap_or("diff facts");
    let symbols = lines
        .iter()
        .find_map(|line| line.strip_prefix("top changed symbols: "))
        .filter(|symbols| *symbols != "none detected")
        .map(|symbols| format!(" · symbols: {symbols}"))
        .unwrap_or_default();
    format!("{churn}{symbols}")
}

fn zen_stop_comment_lines(
    session: &ReviewSession,
    stop: &super::chunks::WalkthroughRow,
) -> Vec<String> {
    super::zen::comments_for_stop(&session.comments, stop)
        .into_iter()
        .map(|comment| {
            let state = comment.state.label();
            let id = truncate_tail(&comment.id, 8);
            let first_line = comment.body.lines().next().unwrap_or_default().trim();
            format!("comment [{state}] {id}: {first_line}")
        })
        .collect()
}

/// The zen focus card: a full-screen stop showing only the critical lines
/// (extracted and vertically centered) with the agent's explanation as the
/// co-star. One object of attention per stop — pop in, understand, move on.
/// `tab` drops into the full dimmed diff when surrounding context is needed.
fn draw_zen_stop(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    zen: &ZenState,
    stop: &super::chunks::WalkthroughRow,
    keymap: &KeyMap,
) {
    frame.render_widget(Clear, area);
    let initial_inner = slide_inner(area);
    let (stop_number, stop_total) = zen.chunk_position();
    if initial_inner.height < 8 {
        frame.render_widget(
            Paragraph::new(zen_focus_header_lines(zen, stop)).wrap(Wrap { trim: false }),
            initial_inner,
        );
        return;
    }

    let rows = session.diff_rows_for_selected_file();
    let cursor = (session.focus == Focus::Diff).then_some(session.diff_cursor);
    let probe_rows = ((initial_inner.height as usize) / 2).clamp(5, 18);
    let (probe_indices, _, _) = zen_excerpt_indices(&rows, stop, probe_rows, cursor);
    let probe_width = probe_indices
        .iter()
        .map(|&index| line_width(&unified_row_line(session, &rows[index], index, 0)))
        .max()
        .unwrap_or(0);
    let content_width = measured_content_width(area.width, probe_width);
    let inner = slide_column(area, content_width);

    let (mut explanation, explanation_title) = if zen.source == super::zen::ZenSource::Files {
        (
            stop.rationale
                .clone()
                .unwrap_or_else(|| "largest changed hunk selected for review".to_owned()),
            " why this matters ",
        )
    } else {
        (
            stop.explanation
                .clone()
                .or_else(|| stop.rationale.clone())
                .unwrap_or_else(|| {
                    "(the agent gave no explanation for this stop — @ summons one)".to_owned()
                }),
            " why this matters ",
        )
    };
    if let Some((part, total)) = stop.part_position
        && part > 1
        && total > 1
    {
        explanation = format!("explanation with part 1/{total}");
    }

    let text_width = inner.width.max(20) as usize;
    let comment_lines = zen_stop_comment_lines(session, stop);
    let sibling_line = sibling_parts_line(zen, stop, inner.width.saturating_sub(2) as usize);
    let position = stop
        .part_position
        .map(|(part, total)| format!(" (part {part}/{total})"))
        .unwrap_or_default();
    let mut prose_lines = vec![
        Line::from(vec![Span::styled(
            format!(
                "stop {stop_number}/{stop_total} · {} · {}",
                chunk_row_location_width(stop, text_width / 2),
                zen_progress_label(zen)
            ),
            Style::default().fg(Color::DarkGray),
        )]),
        Line::from(""),
        Line::from(Span::styled(
            format!("{}{position}", stop.title),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    let abbreviated_part = stop
        .part_position
        .is_some_and(|(part, total)| part > 1 && total > 1);
    let mut explanation_lines = Vec::new();
    if abbreviated_part {
        if let Some((part, total)) = stop.part_position {
            let sibling = sibling_line
                .clone()
                .unwrap_or_else(|| "other parts: part 1".to_owned());
            explanation_lines.push(Line::from(Span::styled(
                format!("prose on part 1 · part {part}/{total} · {sibling}"),
                Style::default().fg(Color::DarkGray),
            )));
        }
    } else if let Some(why) = stop.rationale.as_ref().filter(|s| !s.trim().is_empty()) {
        explanation_lines.push(Line::from(Span::styled(
            explanation_title.trim(),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::ITALIC),
        )));
        explanation_lines.push(Line::from(Span::styled(
            why.clone(),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::ITALIC),
        )));
    }
    if !abbreviated_part
        && let Some(body_text) = stop.explanation.as_ref().filter(|s| !s.trim().is_empty())
    {
        if !explanation_lines.is_empty() {
            explanation_lines.push(Line::from(""));
        }
        push_text_lines(
            &mut explanation_lines,
            body_text,
            Style::default().fg(Color::Gray),
        );
    } else if explanation_lines.is_empty() {
        explanation_lines.push(Line::from(Span::styled(
            explanation,
            Style::default().fg(Color::DarkGray),
        )));
    }
    if !abbreviated_part && let Some(sibling) = sibling_line {
        explanation_lines.push(Line::from(Span::styled(
            sibling,
            Style::default().fg(Color::DarkGray),
        )));
    }
    for comment in comment_lines {
        explanation_lines.push(Line::from(Span::styled(
            comment,
            Style::default().fg(Color::Blue),
        )));
    }
    prose_lines.extend(explanation_lines);
    let prose_height: usize = prose_lines
        .iter()
        .map(|line| wrapped_line_height(line, text_width))
        .sum();
    let reserved_after_excerpt = if stop.artifacts.is_empty() { 0 } else { 2 } + 1;
    let max_excerpt = (inner.height as usize)
        // One row separates prose from code, one row is the divider after the
        // excerpt, and one row of slack keeps the static footer/live footer
        // from being the thing that clips the trailer at exact boundaries.
        .saturating_sub(prose_height + reserved_after_excerpt + 2)
        .clamp(3, 24);
    let (mut indices, wandered, clipped) = zen_excerpt_indices(&rows, stop, max_excerpt, cursor);
    let mut excerpt: Vec<Line<'static>> = Vec::new();
    if indices.is_empty() {
        excerpt.push(Line::from(Span::styled(
            "  (no diff lines to excerpt — tab shows the full file)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    let last_excerpt_lineno = indices
        .last()
        .and_then(|&index| rows.get(index))
        .and_then(|row| row.new_lineno.or(row.old_lineno));
    let mut forced_clip = clipped.or_else(|| {
        let end = stop.part.as_ref()?.end_line?;
        let last = last_excerpt_lineno?;
        (last < end).then_some((end.saturating_sub(last), end))
    });
    if forced_clip.is_some() && indices.len() >= max_excerpt {
        indices.pop();
        if let Some(end) = stop.part.as_ref().and_then(|part| part.end_line)
            && let Some(last) = indices
                .last()
                .and_then(|&index| rows.get(index))
                .and_then(|row| row.new_lineno.or(row.old_lineno))
        {
            forced_clip = Some((end.saturating_sub(last), end));
        }
    }
    for index in indices {
        excerpt.push(unified_row_line(session, &rows[index], index, 0));
    }
    if let Some((more, end)) = forced_clip {
        excerpt.push(Line::from(Span::styled(
            format!(
                "  … {more} more lines through {end} — {}/{}",
                keymap.hint(Action::MoveDown),
                keymap.hint(Action::MoveUp),
            ),
            Style::default().fg(Color::DarkGray),
        )));
    }
    let mut lines = prose_lines;
    lines.push(Line::from(""));
    lines.extend(excerpt);
    lines.push(Line::from(Span::styled(
        "─".repeat(text_width),
        Style::default().fg(Color::DarkGray),
    )));
    if !stop.artifacts.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "e exhibits: {}",
                stop.artifacts
                    .iter()
                    .map(|artifact| artifact.title.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Style::default().fg(Color::DarkGray),
        )));
    }
    if wandered {
        lines.push(Line::from(Span::styled(
            "off the stop — . refocuses",
            Style::default().fg(Color::Magenta),
        )));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// Header lines for the focus card backdrop: the progress dots.
fn zen_focus_header_lines(
    zen: &ZenState,
    stop: &super::chunks::WalkthroughRow,
) -> Vec<Line<'static>> {
    let position = stop
        .part_position
        .map(|(part, total)| format!(" (part {part}/{total})"))
        .unwrap_or_default();
    vec![
        zen_progress_line(zen),
        Line::from(vec![
            Span::styled(
                format!("{}{position}", truncate_tail(&stop.title, 48)),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                chunk_row_location_width(stop, 48),
                Style::default().fg(Color::Cyan),
            ),
        ]),
        Line::from(""),
    ]
}

fn sibling_parts_line(
    zen: &ZenState,
    stop: &super::chunks::WalkthroughRow,
    max_width: usize,
) -> Option<String> {
    let parts = zen
        .stops
        .iter()
        .filter_map(|candidate| match candidate {
            ZenStop::Chunk(row) if row.source_id == stop.source_id && row.part != stop.part => {
                Some(format!(
                    "{}{}",
                    chunk_row_location_width(row, 40),
                    row.part_position
                        .map(|(part, total)| format!(" ({part}/{total})"))
                        .unwrap_or_default()
                ))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    (!parts.is_empty())
        .then(|| truncate_tail(&format!("other parts: {}", parts.join(" · ")), max_width))
}

/// The walkthrough progress strip: a dot per spotlight stop, grouped by
/// chapter (`▎` bars introduce each chapter), the current station bold.
fn zen_progress_line(zen: &ZenState) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (index, stop) in zen.stops.iter().take(40).enumerate() {
        let glyph = match stop {
            super::zen::ZenStop::Chapter(_) => "▎",
            super::zen::ZenStop::Chunk(_) if index <= zen.index => "● ",
            super::zen::ZenStop::Chunk(_) => "○ ",
        };
        spans.push(if index == zen.index {
            Span::styled(
                glyph,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(glyph, Style::default().fg(Color::DarkGray))
        });
    }
    Line::from(spans)
}

/// Rows worth excerpting for a stop: diff lines whose line number falls in
/// the stop's range ± context, or the first changed lines for whole-file
/// stops. When the cursor has wandered outside that window, the excerpt
/// becomes a sliding window centered on the cursor instead — the code
/// scrolls with j/k rather than letting the cursor vanish — and the second
/// return value reports the wander so the UI can offer a refocus hint.
/// Indices into the selected file's diff rows.
fn zen_excerpt_indices(
    rows: &[DiffRow],
    stop: &super::chunks::WalkthroughRow,
    max_rows: usize,
    cursor: Option<usize>,
) -> (Vec<usize>, bool, Option<(usize, usize)>) {
    const CONTEXT: usize = 2;
    let range = stop.part.as_ref().and_then(|part| {
        part.start_line
            .map(|start| (start, part.end_line.unwrap_or(start)))
    });
    let mut indices: Vec<usize> = match range {
        Some((start, end)) => {
            let lo = if max_rows <= CONTEXT + 1 {
                start
            } else {
                start.saturating_sub(CONTEXT)
            };
            let hi = end + CONTEXT;
            rows.iter()
                .enumerate()
                .filter(|(_, row)| {
                    matches!(row.kind, DiffRowKind::DiffLine(_))
                        && row
                            .new_lineno
                            .or(row.old_lineno)
                            .is_some_and(|line| line >= lo && line <= hi)
                })
                .map(|(index, _)| index)
                .collect()
        }
        None => rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                matches!(
                    row.kind,
                    DiffRowKind::DiffLine(DiffLineKind::Added | DiffLineKind::Removed)
                )
            })
            .map(|(index, _)| index)
            .collect(),
    };
    let max_rows = max_rows.max(1);
    if let Some((start, end)) = range
        && indices.len() > max_rows
        && let Some(anchor_pos) = indices.iter().position(|&index| {
            rows.get(index).is_some_and(|row| {
                row.new_lineno
                    .or(row.old_lineno)
                    .is_some_and(|line| line >= start && line <= end)
            })
        })
    {
        indices = indices[anchor_pos..].to_vec();
    }
    let clipped = if let Some((_, end)) = range {
        if indices.len() > max_rows {
            // The trailer is part of the excerpt's vertical budget. Reserve a
            // row for it up front; otherwise exact-fit focus cards render a
            // full code window and the `… N more lines through <end>` cue is
            // pushed below the slide/footer.
            let visible_rows = max_rows.saturating_sub(1).max(1);
            let more = indices.len().saturating_sub(visible_rows);
            indices.truncate(visible_rows);
            Some((more, end))
        } else {
            indices.truncate(max_rows);
            let last_visible_line = indices
                .last()
                .and_then(|&index| rows.get(index))
                .and_then(|row| row.new_lineno.or(row.old_lineno));
            last_visible_line.filter(|line| *line < end).map(|line| {
                if indices.len() >= max_rows {
                    let visible_rows = max_rows.saturating_sub(1).max(1);
                    indices.truncate(visible_rows);
                    let line = indices
                        .last()
                        .and_then(|&index| rows.get(index))
                        .and_then(|row| row.new_lineno.or(row.old_lineno))
                        .unwrap_or(line);
                    (end.saturating_sub(line), end)
                } else {
                    (end.saturating_sub(line), end)
                }
            })
        }
    } else {
        indices.truncate(max_rows);
        None
    };

    // Cursor off the stop: follow it with a window of the same size.
    if let Some(cursor) = cursor
        && !indices.contains(&cursor)
        && rows
            .get(cursor)
            .is_some_and(|row| matches!(row.kind, DiffRowKind::DiffLine(_)))
    {
        let all: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row.kind, DiffRowKind::DiffLine(_)))
            .map(|(index, _)| index)
            .collect();
        if let Some(position) = all.iter().position(|&index| index == cursor) {
            let start = position.saturating_sub(max_rows / 2);
            let end = (start + max_rows).min(all.len());
            let start = end.saturating_sub(max_rows);
            return (all[start..end].to_vec(), true, None);
        }
    }
    (indices, false, clipped)
}

/// A modal exhibit viewer layered over the focus card: one agent-produced
/// artifact (usage example, captured output, diagram) rendered verbatim and
/// scrollable. `h`/`l` cycle when the stop carries several exhibits.
fn draw_zen_artifact(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    zen: &ZenState,
    index: usize,
    scroll: u16,
    keymap: &KeyMap,
) {
    let Some(stop) = zen.current() else {
        return;
    };
    let artifacts = super::zen::stop_artifacts(stop);
    let Some(artifact) = artifacts.get(index.min(artifacts.len().saturating_sub(1))) else {
        return;
    };

    // Size the popup to the exhibit (plus chrome), bounded by the screen.
    let widest = artifact
        .body
        .lines()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0) as u16;
    let max_width = area.width.saturating_sub(6).max(20);
    let width = (widest + 4).clamp(40.min(max_width), max_width);
    let body_lines = artifact.body.lines().count() as u16;
    let max_height = area.height.saturating_sub(2).max(6);
    let height = (body_lines + 3).clamp(6.min(max_height), max_height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };

    frame.render_widget(Clear, popup);
    let mut title_spans = vec![
        Span::styled(
            format!(" {} ", artifact.title),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("· {} ", artifact.kind.label()),
            Style::default().fg(Color::Magenta),
        ),
    ];
    if artifacts.len() > 1 {
        title_spans.push(Span::styled(
            format!(
                "· {}/{} ({}/{} switch) ",
                index + 1,
                artifacts.len(),
                keymap.hint(Action::ZenArtifactPrevious),
                keymap.hint(Action::ZenArtifactNext),
            ),
            Style::default().fg(Color::Cyan),
        ));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Magenta))
        .title(Line::from(title_spans))
        .title_bottom(Line::from(Span::styled(
            format!(
                " {}/{} scroll · {}/{} close ",
                keymap.hint(Action::PopupMoveDown),
                keymap.hint(Action::PopupMoveUp),
                keymap.hint(Action::PopupClose),
                keymap.hint(Action::PopupCloseQ),
            ),
            Style::default().fg(Color::DarkGray),
        )));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // Clamp so scrolling stops at the last line instead of a blank void.
    let max_scroll = (artifact.body.lines().count() as u16).saturating_sub(inner.height);
    frame.render_widget(
        Paragraph::new(artifact.body.clone())
            .style(Style::default().fg(Color::Gray))
            .block(Block::default().padding(Padding::horizontal(1)))
            .scroll((scroll.min(max_scroll), 0)),
        inner,
    );
}

/// The zen glance board: everything not worth a full stop — glance chunks
/// and uncovered files — on one skimmable screen. `a` acknowledges the lot.
fn draw_zen_glance(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    zen: &ZenState,
    keymap: &KeyMap,
) {
    frame.render_widget(Clear, area);
    let inner = slide_inner(area);

    let curated_rows = zen
        .glance_rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.part.as_ref().is_none_or(|part| part.path != row.title))
        .collect::<Vec<_>>();
    let auto_rows = zen
        .glance_rows
        .iter()
        .filter(|row| row.part.as_ref().is_some_and(|part| part.path == row.title))
        .collect::<Vec<_>>();
    let glance_groups = grouped_glance_rows(&zen.glance_rows);
    let selected_group = glance_groups
        .iter()
        .position(|group| group.indices.contains(&zen.glance_selected))
        .unwrap_or(0);
    let fixed_lines = 5usize;
    let list_height = (inner.height as usize).saturating_sub(fixed_lines).max(1);
    let window = picker_visible_window(selected_group, glance_groups.len(), list_height);

    let mut lines = vec![
        Line::from(Span::styled(
            "At a glance",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!(
                "{} curated · {} files not toured",
                curated_rows.len(),
                auto_rows.len()
            ),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
    ];
    if window.hidden_above > 0 {
        lines.push(Line::from(Span::styled(
            format!("  ↑ {} more", window.hidden_above),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(Span::styled(
        "curated",
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    )));
    let mut rendered = 0usize;
    for (group_index, group) in glance_groups
        .iter()
        .enumerate()
        .skip(window.start)
        .take(window.end.saturating_sub(window.start))
    {
        let row = group.rows[0];
        if row.part.as_ref().is_some_and(|part| part.path == row.title) {
            continue;
        }
        rendered += 1;
        let selected = group_index == selected_group;
        let marker = if selected { "›" } else { " " };
        let viewed = group
            .rows
            .iter()
            .all(|row| super::zen::glance_row_viewed(session, row));
        let check = if viewed { "✓" } else { "•" };
        let style = if selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else if viewed {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Gray)
        };
        let rationale = row
            .rationale
            .as_deref()
            .filter(|rationale| !rationale.trim().is_empty())
            .map(|rationale| format!(" · {rationale}"))
            .unwrap_or_default();
        let locations = group
            .rows
            .iter()
            .map(|row| chunk_row_location_width(row, 46))
            .collect::<Vec<_>>()
            .join(" · ");
        let title = if group.rows.len() == 1
            && row.part.as_ref().is_some_and(|part| part.path == row.title)
        {
            String::new()
        } else {
            format!("{} ", row.title)
        };
        let row_width = inner.width.saturating_sub(4) as usize;
        let detail = truncate_tail(&format!("{title}{rationale}"), row_width);
        lines.push(Line::from(vec![
            Span::styled(format!("{marker} "), style),
            Span::styled(format!("{check} "), Style::default().fg(Color::Green)),
            Span::styled(
                truncate_tail(&locations, row_width),
                Style::default().fg(Color::Cyan),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(detail, Style::default().fg(Color::DarkGray)),
        ]));
    }
    if rendered == 0 {
        lines.push(Line::from(Span::styled(
            "  (none)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("not toured — {} files", auto_rows.len()),
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    )));
    for line in glance_auto_role_lines(&auto_rows, inner.width.saturating_sub(4) as usize) {
        lines.push(Line::from(Span::styled(
            format!("  {line}"),
            Style::default().fg(Color::Gray),
        )));
    }
    if window.hidden_below > 0 {
        lines.push(Line::from(Span::styled(
            format!("  ↓ {} more", window.hidden_below),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "{}/{} select · {} dives to location · {} ends tour",
            keymap.hint(Action::PopupMoveDown),
            keymap.hint(Action::PopupMoveUp),
            keymap.hint(Action::PopupSelect),
            keymap.hint(Action::PopupClose),
        ),
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

struct GlanceGroup<'a> {
    indices: Vec<usize>,
    rows: Vec<&'a super::chunks::WalkthroughRow>,
}

fn grouped_glance_rows(rows: &[super::chunks::WalkthroughRow]) -> Vec<GlanceGroup<'_>> {
    let mut groups: Vec<GlanceGroup<'_>> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if row.part_position.is_some()
            && let Some(group) = groups.iter_mut().find(|group| {
                group
                    .rows
                    .first()
                    .is_some_and(|first| first.source_id == row.source_id)
            })
        {
            group.indices.push(index);
            group.rows.push(row);
            continue;
        }
        groups.push(GlanceGroup {
            indices: vec![index],
            rows: vec![row],
        });
    }
    groups
}

fn glance_auto_role_lines(
    rows: &[&super::chunks::WalkthroughRow],
    max_width: usize,
) -> Vec<String> {
    let mut by_role = std::collections::BTreeMap::<String, Vec<String>>::new();
    for row in rows {
        let path = row
            .part
            .as_ref()
            .map(|part| part.path.clone())
            .unwrap_or_else(|| row.title.clone());
        let role = row
            .rationale
            .as_deref()
            .unwrap_or("other")
            .split('·')
            .next()
            .unwrap_or("other")
            .trim()
            .to_owned();
        by_role.entry(role).or_default().push(path);
    }
    by_role
        .into_iter()
        .map(|(role, paths)| {
            let names = paths
                .iter()
                .map(|p| distinguish_path_tail(p))
                .collect::<Vec<_>>()
                .join(" · ");
            truncate_tail(&format!("{role} ({}): {names}", paths.len()), max_width)
        })
        .collect()
}

fn distinguish_path_tail(path: &str) -> String {
    let mut parts = path.rsplit('/');
    let file = parts.next().unwrap_or(path);
    if let Some(parent) = parts.next() {
        format!("{parent}/{file}")
    } else {
        file.to_owned()
    }
}

fn draw_draft_list_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    list: &DraftListState,
    keymap: &KeyMap,
) {
    let popup = centered_rect(82, 60, area);
    frame.render_widget(Clear, popup);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 3usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window = picker_visible_window(list.selected, list.drafts.len(), list_height);

    let mut lines = vec![Line::from(Span::styled(
        "Agent-drafted comments awaiting your decision",
        Style::default().fg(Color::DarkGray),
    ))];
    if list.drafts.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no pending drafts",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines.extend(
            list.drafts
                .iter()
                .enumerate()
                .skip(visible_window.start)
                .take(visible_window.end.saturating_sub(visible_window.start))
                .map(|(index, draft)| {
                    let selected = index == list.selected;
                    let marker = if selected { "›" } else { " " };
                    let style = if selected {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    let location = match draft.line {
                        Some(line) => format!("{}:{line}", draft.path),
                        None => draft.path.clone(),
                    };
                    let summary = draft
                        .body
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or("(empty draft)")
                        .trim()
                        .to_owned();
                    Line::from(vec![
                        Span::styled(format!("{marker} "), style),
                        Span::styled(
                            format!("[{:^8}] ", draft.state.label()),
                            Style::default().fg(Color::Magenta),
                        ),
                        Span::styled(format!("{location} "), Style::default().fg(Color::Cyan)),
                        Span::styled(summary, style),
                    ])
                }),
        );
        if visible_window.hidden_below > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more", visible_window.hidden_below),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "{}/{} move · {} accept · {} edit then accept · {} discard · {} close",
            keymap.hint(Action::PopupMoveDown),
            keymap.hint(Action::PopupMoveUp),
            keymap.hint(Action::DraftAccept),
            keymap.hint(Action::DraftEdit),
            keymap.hint(Action::DraftDiscard),
            keymap.hint(Action::PopupClose),
        ),
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("agent drafts"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn flag_priority_style(priority: crate::agent::FlagPriority) -> Style {
    match priority {
        crate::agent::FlagPriority::Critical => {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        }
        crate::agent::FlagPriority::High => Style::default().fg(Color::Red),
        crate::agent::FlagPriority::Medium => Style::default().fg(Color::Yellow),
        crate::agent::FlagPriority::Low => Style::default().fg(Color::DarkGray),
    }
}

fn draw_operation_picker_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    picker: &OperationPickerState,
    keymap: &KeyMap,
) {
    let popup = centered_rect(80, 60, area);
    frame.render_widget(Clear, popup);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 3usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window =
        picker_visible_window(picker.selected, picker.operations.len(), list_height);

    let mut lines = vec![Line::from(Span::styled(
        "Compare against a prior operation: unchanged files are marked caught up",
        Style::default().fg(Color::DarkGray),
    ))];
    if picker.operations.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no operations",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines.extend(
            picker
                .operations
                .iter()
                .enumerate()
                .skip(visible_window.start)
                .take(visible_window.end.saturating_sub(visible_window.start))
                .map(|(index, operation)| {
                    let selected = index == picker.selected;
                    let marker = if selected { "›" } else { " " };
                    let style = if selected {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    let description = if operation.description.is_empty() {
                        "(no description)"
                    } else {
                        &operation.description
                    };
                    let prefix_width = 1 + 1 + 14 + 18;
                    let description_width = popup
                        .width
                        .saturating_sub(2)
                        .saturating_sub(prefix_width)
                        .max(1) as usize;
                    let description = truncate_middle(description, description_width);
                    Line::from(vec![
                        Span::styled(format!("{marker} {:<14}", operation.operation_id), style),
                        Span::styled(
                            format!("{:<18}", operation.time),
                            Style::default().fg(Color::Cyan),
                        ),
                        Span::styled(description, style),
                    ])
                }),
        );
        if visible_window.hidden_below > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more", visible_window.hidden_below),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        picker
            .preview
            .as_deref()
            .unwrap_or("select an operation to preview catch-up"),
        Style::default().fg(Color::Yellow),
    )));
    lines.push(Line::from(Span::styled(
        format!(
            "{}/{} move · {} apply · {} cancel",
            keymap.hint(Action::PopupMoveDown),
            keymap.hint(Action::PopupMoveUp),
            keymap.hint(Action::PopupSelect),
            keymap.hint(Action::PopupClose),
        ),
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title("prior operations"),
        ),
        popup,
    );
}

fn draw_file_search_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    search: &FileSearchState,
    keymap: &KeyMap,
) {
    let popup = centered_rect(72, 60, area);
    frame.render_widget(Clear, popup);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 3usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window = picker_visible_window(search.selected, search.filtered.len(), list_height);

    let mut lines = vec![Line::from(vec![
        Span::styled("search: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            if search.query.is_empty() {
                "type to fuzzy match files".to_owned()
            } else {
                search.query.clone()
            },
            if search.query.is_empty() {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            },
        ),
    ])];
    if search.filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matching files",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines.extend(
            search
                .filtered
                .iter()
                .skip(visible_window.start)
                .take(visible_window.end.saturating_sub(visible_window.start))
                .enumerate()
                .map(|(visible_index, row_index)| {
                    let index = visible_window.start + visible_index;
                    let row = &search.files[*row_index];
                    let selected = index == search.selected;
                    let marker = if selected { "›" } else { " " };
                    let viewed_mark = if row.viewed { "✓" } else { "•" };
                    let style = if selected {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else if row.viewed {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    Line::from(vec![
                        Span::styled(format!("{marker} "), style),
                        Span::styled(viewed_mark, Style::default().fg(Color::Green)),
                        Span::styled(format!(" {}", row.path), style),
                    ])
                }),
        );
        if visible_window.hidden_below > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more", visible_window.hidden_below),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "type fuzzy filter · {}/{} move · {} open file · {} cancel",
            keymap.hint(Action::TargetPickerMoveDown),
            keymap.hint(Action::TargetPickerMoveUp),
            keymap.hint(Action::PopupSelect),
            keymap.hint(Action::PopupClose),
        ),
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("file search"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn draw_symbol_outline_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    outline: &SymbolOutlineState,
    keymap: &KeyMap,
) {
    let popup = centered_rect(60, 50, area);
    frame.render_widget(Clear, popup);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 2usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window =
        picker_visible_window(outline.selected, outline.targets.len(), list_height);

    let mut lines = Vec::new();
    if outline.targets.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no changed symbols",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines.extend(
            outline
                .targets
                .iter()
                .enumerate()
                .skip(visible_window.start)
                .take(visible_window.end.saturating_sub(visible_window.start))
                .map(|(index, target)| {
                    let selected = index == outline.selected;
                    let marker = if selected { "›" } else { " " };
                    let style = if selected {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    Line::from(Span::styled(format!("{marker} {}", target.label), style))
                }),
        );
        if visible_window.hidden_below > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more", visible_window.hidden_below),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        list_popup_hint("", keymap, "jump")
            .trim_start_matches(" · ")
            .to_owned(),
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("changed symbols"),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn comment_list_location(comment: &Comment) -> String {
    let Some(path) = comment.path.as_deref() else {
        return "general".to_owned();
    };
    match (comment.line, comment.end_line) {
        (Some(line), Some(end_line)) => format!("{path}:{line}-{end_line}"),
        (Some(line), None) => format!("{path}:{line}"),
        _ => path.to_owned(),
    }
}

fn draw_comment_list_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    list: &CommentListState,
    keymap: &KeyMap,
) {
    let popup = centered_rect(80, 60, area);
    frame.render_widget(Clear, popup);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 2usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window = picker_visible_window(list.selected, session.comments.len(), list_height);

    let mut lines = Vec::new();
    if session.comments.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no comments",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines.extend(
            session
                .comments
                .iter()
                .enumerate()
                .skip(visible_window.start)
                .take(visible_window.end.saturating_sub(visible_window.start))
                .map(|(index, comment)| {
                    let selected = index == list.selected;
                    let marker = if selected { "›" } else { " " };
                    let style = if selected {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    let location = comment_list_location(comment);
                    let summary = comment
                        .body
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or("(empty comment)")
                        .trim()
                        .to_owned();
                    let mut spans = vec![
                        Span::styled(format!("{marker} "), style),
                        Span::styled(
                            format!("[{:^8}] ", comment.state.label()),
                            comment_state_style(comment.state),
                        ),
                    ];
                    spans.extend(comment_badge_spans(comment));
                    spans.extend([
                        Span::styled(format!("{location} "), Style::default().fg(Color::Cyan)),
                        Span::styled(summary, style),
                    ]);
                    Line::from(spans)
                }),
        );
        if visible_window.hidden_below > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more", visible_window.hidden_below),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "{}/{} move · {} general · {} jump · {} edit · {} state · {} ready drafts · {} action · {} kind · {} delete · {} close",
            keymap.hint(Action::PopupMoveDown),
            keymap.hint(Action::PopupMoveUp),
            keymap.hint(Action::CommentListNewGeneral),
            keymap.hint(Action::PopupSelect),
            keymap.hint(Action::EditComment),
            keymap.hint(Action::CycleCommentState),
            keymap.hint(Action::CommentListReady),
            keymap.hint(Action::CommentListCycleIntent),
            keymap.hint(Action::CommentListCycleKind),
            keymap.hint(Action::DeleteComment),
            keymap.hint(Action::PopupClose),
        ),
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("comments"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn draw_open_work_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    list: &OpenWorkListState,
    keymap: &KeyMap,
) {
    let popup = centered_rect(82, 60, area);
    frame.render_widget(Clear, popup);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 2usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window = picker_visible_window(list.selected, list.rows.len(), list_height);

    let mut lines = Vec::new();
    if list.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no open action items or todo feedback",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(Color::DarkGray),
            )));
        }
        for (index, row) in list
            .rows
            .iter()
            .enumerate()
            .skip(visible_window.start)
            .take(visible_window.end.saturating_sub(visible_window.start))
        {
            let selected = index == list.selected;
            let marker = if selected { "›" } else { " " };
            let style = if selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(open_work_line(session, row, marker, style));
        }
        if visible_window.hidden_below > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more", visible_window.hidden_below),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        list_popup_hint("", keymap, "jump")
            .trim_start_matches(" · ")
            .to_owned(),
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Action items & feedback ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, popup);
}

fn open_work_line(
    session: &ReviewSession,
    row: &OpenWorkRow,
    marker: &str,
    style: Style,
) -> Line<'static> {
    match row {
        OpenWorkRow::ActionItem {
            title,
            action,
            target,
            ..
        } => {
            let mut spans = vec![
                Span::styled(format!("{marker} "), style),
                Span::styled("[item] ", Style::default().fg(Color::Yellow)),
            ];
            if let Some(action) = action
                && *action != crate::state::ActionIntent::None
            {
                spans.push(Span::styled(
                    format!("[{}] ", action_intent_label(*action)),
                    Style::default().fg(Color::Magenta),
                ));
            }
            spans.extend([
                Span::styled(
                    format!("{} ", action_item_location(target.as_ref().as_ref())),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(title.clone(), style),
            ]);
            Line::from(spans)
        }
        OpenWorkRow::EvidenceComment { id } | OpenWorkRow::TodoComment { id } => {
            let nested = matches!(row, OpenWorkRow::EvidenceComment { .. });
            let Some(comment) = session.comments.iter().find(|comment| comment.id == *id) else {
                return Line::from(Span::styled(
                    format!(
                        "{marker} {}missing feedback {id}",
                        if nested { "  └ " } else { "" }
                    ),
                    Style::default().fg(Color::DarkGray),
                ));
            };
            let summary = comment
                .body
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("(empty comment)")
                .trim()
                .to_owned();
            let mut spans = vec![
                Span::styled(
                    format!("{marker} {}", if nested { "  └ " } else { "" }),
                    style,
                ),
                Span::styled(
                    if nested { "[evidence] " } else { "[feedback] " },
                    Style::default().fg(Color::Blue),
                ),
                Span::styled(
                    format!("{} ", comment_list_location(comment)),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    format!("[{}] ", comment.state.label()),
                    comment_state_style(comment.state),
                ),
            ];
            spans.extend(comment_badge_spans(comment));
            spans.push(Span::styled(summary, style));
            Line::from(spans)
        }
    }
}

fn action_item_location(target: Option<&ReviewTarget>) -> String {
    let Some(target) = target else {
        return "general".to_owned();
    };
    let Some(path) = target.file.as_deref() else {
        return "general".to_owned();
    };
    match (target.line, target.end_line) {
        (Some(start), Some(end)) if end != start => format!("{path}:{start}-{end}"),
        (Some(line), _) => format!("{path}:{line}"),
        _ => path.to_owned(),
    }
}

/// Wall-clock label for an activity event. Tests format in UTC so snapshot
/// output does not depend on the machine (or sandbox) timezone; production
/// uses the local timezone.
fn activity_event_time(timestamp: &chrono::DateTime<chrono::Utc>) -> String {
    #[cfg(test)]
    {
        timestamp.format("%H:%M:%S").to_string()
    }
    #[cfg(not(test))]
    {
        timestamp
            .with_timezone(&chrono::Local)
            .format("%H:%M:%S")
            .to_string()
    }
}

fn draw_activity_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    tui_state: &TuiState,
    state: &ActivityListState,
) {
    let popup = centered_rect(72, 60, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(" activity ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .padding(Padding::horizontal(1));
    let items: Vec<ListItem<'static>> = tui_state
        .activity
        .iter()
        .rev()
        .map(|event| {
            let time = activity_event_time(&event.timestamp);
            let message = if event.count > 1 {
                format!("{} {}×", event.message, event.count)
            } else {
                event.message.clone()
            };
            ListItem::new(Line::from(vec![
                Span::styled(time, Style::default().fg(Color::DarkGray)),
                Span::raw("  "),
                Span::raw(truncate_tail(
                    &message,
                    popup.width.saturating_sub(16) as usize,
                )),
            ]))
        })
        .collect();
    let mut list_state = ListState::default();
    if !items.is_empty() {
        list_state.select(Some(state.selected.min(items.len() - 1)));
    }
    let inner = inner_bordered(popup);
    let detail_height = if popup.height >= 18 {
        4
    } else if popup.height >= 12 {
        3
    } else if popup.height >= 8 {
        2
    } else {
        0
    };
    let regions = if detail_height > 0 {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(detail_height)])
            .split(inner)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1)])
            .split(inner)
    };
    frame.render_widget(block, popup);
    let list = List::new(items).highlight_symbol("› ").highlight_style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(list, regions[0], &mut list_state);
    if detail_height > 0
        && let Some(event) = tui_state.activity.iter().rev().nth(state.selected)
    {
        let detail = if event.count > 1 {
            format!("{} {}×", event.message, event.count)
        } else {
            event.message.clone()
        };
        let paragraph = Paragraph::new(detail)
            .style(Style::default().fg(Color::Gray))
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, regions[1]);
    }
}

fn notice_message_for_width(message: &str, width: u16) -> String {
    const CUE: &str = " · ctrl-a for detail";
    let width = width as usize;
    if message.width() <= width {
        return message.to_owned();
    }
    if width <= CUE.width() + 1 {
        return truncate_tail(message, width);
    }
    format!("{}{}", truncate_tail(message, width - CUE.width()), CUE)
}

fn draw_walkthrough_list_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    list: &WalkthroughListState,
    keymap: &KeyMap,
) {
    let popup = centered_rect(84, 60, area);
    frame.render_widget(Clear, popup);
    let mut lines = Vec::new();
    for (index, id) in list.step_ids.iter().enumerate() {
        let Some(step) = session
            .sessions
            .iter()
            .flat_map(|s| &s.walkthroughs)
            .flat_map(|w| &w.steps)
            .find(|step| &step.id == id)
        else {
            continue;
        };
        let selected = index == list.selected;
        let style = if selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let marker = if selected { "›" } else { " " };
        let file = step.target.file.as_deref().unwrap_or("<target>");
        let location = match (step.target.line, step.target.end_line) {
            (Some(line), Some(end)) => format!("{file}:{line}-{end}"),
            (Some(line), None) => format!("{file}:{line}"),
            _ => file.to_owned(),
        };
        let title = step.title.as_deref().unwrap_or("(untitled)");
        let why = step
            .why
            .as_deref()
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .trim();
        lines.push(Line::from(vec![
            Span::styled(format!("{marker} {}. ", index + 1), style),
            Span::styled(format!("{title} "), style),
            Span::styled(format!("{location} "), Style::default().fg(Color::Cyan)),
            Span::styled(why.to_owned(), Style::default().fg(Color::DarkGray)),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no walkthrough steps",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "{}/{} move · {} jump · {}/{} reorder · {} delete · {} close",
            keymap.hint(Action::PopupMoveDown),
            keymap.hint(Action::PopupMoveUp),
            keymap.hint(Action::PopupSelect),
            keymap.hint(Action::WalkthroughMoveDown),
            keymap.hint(Action::WalkthroughMoveUp),
            keymap.hint(Action::WalkthroughDelete),
            keymap.hint(Action::PopupClose),
        ),
        Style::default().fg(Color::DarkGray),
    )));
    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Walkthrough ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, popup);
}

fn draw_target_chooser_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    chooser: &TargetChooserState,
    keymap: &KeyMap,
) {
    let popup = centered_rect(84, 64, area);
    frame.render_widget(Clear, popup);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 6usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window =
        picker_visible_window(chooser.selected, chooser.filtered.len(), list_height);

    let mut lines = vec![
        Line::from(vec![
            Span::raw("Choose "),
            Span::styled(
                chooser.selecting.label(),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(" for "),
            Span::styled(
                format!("{}..{}", chooser.current_base, chooser.current_tip),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                " (tab toggles base/tip)",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled("filter: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                if chooser.query.is_empty() {
                    "type to fuzzy match".to_owned()
                } else {
                    chooser.query.clone()
                },
                if chooser.query.is_empty() {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::White)
                },
            ),
        ]),
        Line::from(Span::styled(
            "   change id      bookmarks                 description",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    if chooser.filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matching changes",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines.extend(
            chooser
                .filtered
                .iter()
                .skip(visible_window.start)
                .take(visible_window.end.saturating_sub(visible_window.start))
                .enumerate()
                .map(|(visible_index, row_index)| {
                    let index = visible_window.start + visible_index;
                    let row = &chooser.rows[*row_index];
                    base_picker_row(
                        row,
                        *row_index == 0,
                        index == chooser.selected,
                        row.matches_rev(&chooser.current_base),
                        chooser.tip_matches_row(row, *row_index),
                    )
                }),
        );
        if visible_window.hidden_below > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more", visible_window.hidden_below),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "type fuzzy filter · tab base/tip · {}/{} move · {} use selected · {} cancel",
            keymap.hint(Action::TargetPickerMoveDown),
            keymap.hint(Action::TargetPickerMoveUp),
            keymap.hint(Action::PopupSelect),
            keymap.hint(Action::PopupClose),
        ),
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("target"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PickerVisibleWindow {
    start: usize,
    end: usize,
    hidden_above: usize,
    hidden_below: usize,
}

fn picker_visible_window(selected: usize, total: usize, height: usize) -> PickerVisibleWindow {
    if total == 0 || height == 0 {
        return PickerVisibleWindow {
            start: 0,
            end: 0,
            hidden_above: 0,
            hidden_below: 0,
        };
    }
    if total <= height {
        return PickerVisibleWindow {
            start: 0,
            end: total,
            hidden_above: 0,
            hidden_below: 0,
        };
    }

    let selected = selected.min(total - 1);
    let mut best: Option<(usize, usize, usize)> = None;
    for start in 0..=selected {
        let show_above = usize::from(start > 0);
        let Some(mut data_capacity) = height.checked_sub(show_above) else {
            continue;
        };
        if data_capacity == 0 {
            continue;
        }
        if start + data_capacity < total {
            if data_capacity == 1 {
                continue;
            }
            data_capacity -= 1;
        }
        let end = (start + data_capacity).min(total);
        if selected >= end {
            continue;
        }
        let ideal_start = selected.saturating_sub(data_capacity / 2);
        let distance = start.abs_diff(ideal_start);
        let visible_rows = end - start;
        let score = height.saturating_sub(visible_rows) * 1000 + distance;
        if best.is_none_or(|(_, _, best_score)| score < best_score) {
            best = Some((start, end, score));
        }
    }

    let (start, end, _) = best.unwrap_or((selected, selected + 1, 0));
    PickerVisibleWindow {
        start,
        end,
        hidden_above: start,
        hidden_below: total.saturating_sub(end),
    }
}

fn base_picker_row(
    row: &JjChangeSummary,
    at_tip: bool,
    selected: bool,
    current_base: bool,
    current_tip: bool,
) -> Line<'static> {
    let style = if selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let marker = if selected { "›" } else { " " };
    let base_mark = if current_base { "B" } else { " " };
    let tip_mark = if current_tip { "T" } else { " " };
    let at_mark = if at_tip { "@" } else { " " };
    let description = if row.description.is_empty() {
        "(no description)"
    } else {
        row.title()
    };
    Line::from(vec![
        Span::styled(
            format!(
                "{marker}{base_mark}{tip_mark}{at_mark} {:<13}",
                row.change_id
            ),
            style,
        ),
        Span::styled(
            format!("{:<26}", row.bookmarks),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(description.to_owned(), style),
    ])
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{
        Terminal,
        backend::TestBackend,
        buffer::Buffer,
        style::{Color, Modifier},
    };

    use crate::{
        config::KeybindingsConfig,
        state::Comment,
        syntax::{HighlightKind, SyntaxSpan, SyntaxThemeConfig},
        tui::test_support::snapshot_session,
    };

    fn render_tui_text(session: &ReviewSession, mode: &Mode, width: u16, height: u16) -> String {
        render_tui_text_with_zen(session, mode, None, width, height)
    }

    fn render_tui_buffer_and_cursor(
        session: &ReviewSession,
        mode: &Mode,
        width: u16,
        height: u16,
    ) -> (Buffer, (u16, u16)) {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    session,
                    mode,
                    &keymap,
                    &TuiState::default(),
                    None,
                    None,
                )
            })
            .unwrap();
        let position = terminal.backend().cursor_position();
        (
            terminal.backend().buffer().clone(),
            (position.x, position.y),
        )
    }

    fn buffer_row(buffer: &Buffer, area: Rect, row: u16) -> String {
        (area.x..area.x + area.width)
            .map(|x| buffer[(x, area.y + row)].symbol())
            .collect::<String>()
    }

    fn render_tui_text_with_state(
        session: &ReviewSession,
        mode: &Mode,
        tui_state: &TuiState,
        width: u16,
        height: u16,
    ) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    session,
                    mode,
                    &keymap,
                    tui_state,
                    tui_state.notice.as_ref(),
                    None,
                )
            })
            .unwrap();
        buffer_text(terminal.backend().buffer())
    }

    fn render_tui_text_with_zen(
        session: &ReviewSession,
        mode: &Mode,
        zen: Option<&ZenState>,
        width: u16,
        height: u16,
    ) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        terminal
            .draw(|frame| {
                draw(
                    frame,
                    session,
                    mode,
                    &keymap,
                    &TuiState::default(),
                    None,
                    zen,
                )
            })
            .unwrap();

        buffer_text(terminal.backend().buffer())
    }

    #[test]
    fn popup_footer_uses_effective_configured_hints() {
        let session = snapshot_session("");
        let mode = Mode::CommentList(CommentListState::default());
        let config = KeybindingsConfig {
            popup_move_down: vec!["alt-j".to_owned()],
            popup_move_up: vec!["alt-k".to_owned()],
            comment_list_new_general: vec!["g".to_owned()],
            ..KeybindingsConfig::default()
        };
        let keymap = KeyMap::try_from(&config).unwrap();
        let backend = TestBackend::new(180, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    &session,
                    &mode,
                    &keymap,
                    &TuiState::default(),
                    None,
                    None,
                )
            })
            .unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("alt-j/alt-k move"), "{rendered}");
        assert!(rendered.contains("g general"), "{rendered}");
    }

    fn render_tui_style_runs(
        session: &ReviewSession,
        mode: &Mode,
        width: u16,
        height: u16,
    ) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        terminal
            .draw(|frame| {
                draw(
                    frame,
                    session,
                    mode,
                    &keymap,
                    &TuiState::default(),
                    None,
                    None,
                )
            })
            .unwrap();

        buffer_style_runs(terminal.backend().buffer())
    }

    fn buffer_text(buffer: &Buffer) -> String {
        let area = buffer.area;
        let mut out = String::new();
        for y in area.y..area.y + area.height {
            let mut line = String::new();
            for x in area.x..area.x + area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    fn buffer_style_runs(buffer: &Buffer) -> String {
        let area = buffer.area;
        let mut out = String::new();
        for y in area.y..area.y + area.height {
            let mut x = area.x;
            while x < area.x + area.width {
                let cell = &buffer[(x, y)];
                let style = cell.style();
                if style_is_plain(style) {
                    x += 1;
                    continue;
                }

                let start = x;
                let mut text = String::new();
                while x < area.x + area.width && buffer[(x, y)].style() == style {
                    text.push_str(buffer[(x, y)].symbol());
                    x += 1;
                }
                out.push_str(&format!(
                    "y={y:02} x={start:02}..{end:02} fg={fg:?} bg={bg:?} add={add:?} sub={sub:?} text={text:?}\n",
                    end = x.saturating_sub(1),
                    fg = style.fg,
                    bg = style.bg,
                    add = style.add_modifier,
                    sub = style.sub_modifier,
                ));
            }
        }
        out
    }

    fn style_is_plain(style: Style) -> bool {
        matches!(style.fg, None | Some(Color::Reset))
            && matches!(style.bg, None | Some(Color::Reset))
            && style.add_modifier.is_empty()
            && style.sub_modifier.is_empty()
    }

    #[test]
    fn tui_snapshot_basic_files_and_diff() {
        let session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,4 +1,5 @@
 fn main() {
-    old();
+    new();
+    extra();
 }
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-old title
+new title
"#,
        );
        insta::assert_snapshot!(render_tui_text(&session, &Mode::Normal, 100, 24));
    }

    #[test]
    fn tui_snapshot_help_overlay() {
        let session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,2 +1,2 @@
-    old();
+    new();
"#,
        );

        insta::assert_snapshot!(render_tui_text(&session, &Mode::Help, 110, 32));
    }

    #[test]
    fn help_exposes_wrap_and_horizontal_scroll_actions() {
        let session = snapshot_session("");
        let rendered = render_tui_text(&session, &Mode::Help, 110, 32);

        assert!(rendered.contains("soft wrap, layout, and visual cues"));
        assert!(rendered.contains("horizontal scroll when"));
        assert!(rendered.contains("wrap is off"));
        assert!(rendered.contains("shift-left/shift-right"));
        assert!(!rendered.contains("?  toggle diff soft wrap"));
        assert!(rendered.contains("j/k or page keys scroll"));

        let tui_state = TuiState {
            help_scroll: 30,
            ..TuiState::default()
        };
        let scrolled = render_tui_text_with_state(&session, &Mode::Help, &tui_state, 110, 32);
        assert!(scrolled.contains("agent draft comments") || scrolled.contains("badges"));
    }

    #[test]
    fn tui_snapshot_context_expansion_gaps() {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
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
"#,
        );
        let contents: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        session.store_file_contents("src/app.rs", Some(contents));
        session.toggle_focus();
        // Cursor on the second hunk's first context line, then partially
        // expand the middle gap: two lines revealed above the hunk, three
        // still hidden behind the gap row.
        let rows = session.diff_rows_for_selected_file();
        session.select_diff_row(rows.iter().position(|row| row.text == "line 12").unwrap());
        session.expand_nearest_gap(Some(2));

        insta::assert_snapshot!(render_tui_text(&session, &Mode::Normal, 100, 24));
    }

    #[test]
    fn tui_snapshot_diff_focus_with_range() {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,4 +1,5 @@
 fn main() {
-    old();
+    new();
+    extra();
 }
"#,
        );
        session.toggle_focus();
        session.set_diff_range_selection(3, 4);

        insta::assert_snapshot!(render_tui_text(&session, &Mode::Normal, 100, 18));
    }

    #[test]
    fn tui_snapshot_comment_popup() {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1 +1 @@
-old
+new
"#,
        );
        session.toggle_focus();
        let row = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "new")
            .unwrap();
        session.select_diff_row(row);
        let mode = Mode::CommentInput {
            editor: {
                let mut editor = CommentEditor::new("Looks good\nexcept this line".to_owned());
                editor.cursor = "Looks good\nexcept".len();
                editor
            },
            target: CommentInputTarget::New,
        };

        insta::assert_snapshot!(render_tui_text(&session, &mode, 100, 24));
    }

    #[test]
    fn comment_popup_renders_prewrapped_rows_and_exact_soft_wrap_cursor() {
        let session = snapshot_session("");
        let terminal_area = Rect::new(0, 0, 20, 15);
        let inner = comment_editor_inner(terminal_area);
        let full_row = format!("{}界", "a".repeat(inner.width as usize - 2));
        let text = format!("{full_row}x");
        let mut editor = CommentEditor::new(text);
        editor.cursor = full_row.len();
        let mode = Mode::CommentInput {
            editor,
            target: CommentInputTarget::NewGeneral,
        };

        let (buffer, cursor) = render_tui_buffer_and_cursor(
            &session,
            &mode,
            terminal_area.width,
            terminal_area.height,
        );

        assert!(buffer_row(&buffer, inner, 0).starts_with(&full_row));
        assert!(buffer_row(&buffer, inner, 1).starts_with('x'));
        assert_eq!(cursor, (inner.x, inner.y + 1));
    }

    #[test]
    fn comment_popup_cursor_uses_cjk_combining_and_emoji_display_width() {
        let session = snapshot_session("");
        let terminal_area = Rect::new(0, 0, 50, 15);
        let inner = comment_editor_inner(terminal_area);
        let before = "界e\u{301}👩🏽‍💻";
        let mut editor = CommentEditor::new(format!("{before}x"));
        editor.cursor = before.len();
        let mode = Mode::CommentInput {
            editor,
            target: CommentInputTarget::AcceptDraft { id: "draft".into() },
        };

        let (_, cursor) = render_tui_buffer_and_cursor(
            &session,
            &mode,
            terminal_area.width,
            terminal_area.height,
        );

        assert_eq!(cursor, (inner.x + 5, inner.y));
    }

    #[test]
    fn comment_popup_scrolls_existing_long_text_to_the_cursor() {
        let session = snapshot_session("");
        let terminal_area = Rect::new(0, 0, 40, 10);
        let inner = comment_editor_inner(terminal_area);
        assert_eq!(inner.height, 2);
        let editor =
            CommentEditor::new("line 0\nline 1\nline 2\nline 3\nline 4\nline 5".to_owned());
        let mode = Mode::CommentInput {
            editor,
            target: CommentInputTarget::Edit {
                id: "comment".into(),
            },
        };

        let (buffer, cursor) = render_tui_buffer_and_cursor(
            &session,
            &mode,
            terminal_area.width,
            terminal_area.height,
        );

        assert!(buffer_row(&buffer, inner, 0).starts_with("line 4"));
        assert!(buffer_row(&buffer, inner, 1).starts_with("line 5"));
        assert_eq!(cursor, (inner.x + 6, inner.y + 1));
    }

    #[test]
    fn tui_snapshot_target_chooser() {
        let session = snapshot_session("");
        let mode = Mode::TargetChooser(TargetChooserState::new(
            vec![
                JjChangeSummary {
                    change_id: "abc123".to_owned(),
                    bookmarks: "main".to_owned(),
                    description: "feat: first change".to_owned(),
                },
                JjChangeSummary {
                    change_id: "def456".to_owned(),
                    bookmarks: "feature*".to_owned(),
                    description: "fix: selected change".to_owned(),
                },
            ],
            "def456",
            "@",
        ));

        insta::assert_snapshot!(render_tui_text(&session, &mode, 100, 24));
    }

    #[test]
    fn tui_snapshot_file_search() {
        let session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1 +1 @@
-old
+new
diff --git a/src/tui/render.rs b/src/tui/render.rs
--- a/src/tui/render.rs
+++ b/src/tui/render.rs
@@ -1 +1 @@
-old
+new
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-old
+new
"#,
        );
        let mut search = FileSearchState::new(&session);
        for ch in "rs".chars() {
            search.push_query_char(ch);
        }
        let mode = Mode::FileSearch(search);

        insta::assert_snapshot!(render_tui_text(&session, &mode, 100, 24));
    }

    #[test]
    fn tui_snapshot_comment_list() {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1 +1 @@
-old
+new
"#,
        );
        session.add_comment("File-level note".into());
        session.toggle_focus();
        session.add_comment("Line note that needs fixing".into());
        // UUID ids would leak into the diff gutter; pin them for the snapshot.
        session.comments[0].id = "file-note".to_owned();
        session.comments[1].id = "line-note".to_owned();
        session.comments[1].action = Some(crate::state::ActionIntent::Fix);
        session.comments[1].kind = Some(crate::state::CommentKind::Question);
        let todo_id = session.comments[1].id.clone();
        session.cycle_comment_state(&todo_id);
        let mode = Mode::CommentList(CommentListState { selected: 1 });

        insta::assert_snapshot!(render_tui_text(&session, &mode, 100, 24));
    }

    #[test]
    fn tui_snapshot_open_work() {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1 +1 @@
-old
+new
"#,
        );
        session.add_comment("Draft only".into());
        session.toggle_focus();
        session.add_comment("Branch is untested".into());
        session.add_comment("Please document the fallback".into());
        session.comments[0].state = crate::state::CommentState::Draft;
        session.comments[1].id = "test-evidence".to_owned();
        session.comments[1].action = Some(crate::state::ActionIntent::Test);
        session.comments[1].kind = Some(crate::state::CommentKind::Issue);
        session.comments[1].state = crate::state::CommentState::Todo;
        session.comments[2].id = "standalone-feedback".to_owned();
        session.comments[2].state = crate::state::CommentState::Todo;
        let durable_id = session.comments[1].session_id.clone().unwrap();
        session
            .sessions
            .iter_mut()
            .find(|durable| durable.id == durable_id)
            .unwrap()
            .action_items
            .push(crate::state::ActionItem {
                id: "regression-item".to_owned(),
                title: "Cover fallback with a regression test".to_owned(),
                action: Some(crate::state::ActionIntent::Test),
                target: Some(crate::state::ReviewTarget {
                    file: Some("src/app.rs".to_owned()),
                    line: Some(1),
                    ..crate::state::ReviewTarget::default()
                }),
                comment_ids: vec!["test-evidence".to_owned()],
                ..crate::state::ActionItem::default()
            });
        let mode = Mode::OpenWork(OpenWorkListState::new(&session));

        insta::assert_snapshot!(render_tui_text(&session, &mode, 100, 24));
    }

    #[test]
    fn tui_snapshot_activity_popup() {
        let session = snapshot_session(
            "diff --git a/src/app.rs b/src/app.rs\n--- a/src/app.rs\n+++ b/src/app.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let mut tui_state = TuiState::default();
        let timestamp = chrono::DateTime::parse_from_rfc3339("2026-07-05T12:34:56Z")
            .unwrap()
            .to_utc();
        tui_state.activity.push_back(super::super::ActivityEvent {
            timestamp,
            key: "queue.rs".to_owned(),
            message: "queue.rs updated".to_owned(),
            count: 3,
        });
        tui_state.activity.push_back(super::super::ActivityEvent {
            timestamp,
            key: "@".to_owned(),
            message: "@ moved to abcdef".to_owned(),
            count: 1,
        });
        insta::assert_snapshot!(render_tui_text_with_state(
            &session,
            &Mode::Activity(ActivityListState::new()),
            &tui_state,
            100,
            20
        ));
    }

    #[test]
    fn tui_snapshot_freshness_badges_and_notice() {
        let mut session = snapshot_session(
            "diff --git a/src/app.rs b/src/app.rs\n--- a/src/app.rs\n+++ b/src/app.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-old\n+new\n",
        );
        session.files[0].changed_since_look = true;
        session.files[0].changed_hunks.insert(0);
        if session.files.len() > 1 {
            session.files[1].viewed_stale = true;
        }
        let tui_state = TuiState {
            notice: Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: "repository changed — @ moved to abcdef · change queue updated".to_owned(),
            }),
            ..TuiState::default()
        };
        insta::assert_snapshot!(render_tui_text_with_state(
            &session,
            &Mode::Normal,
            &tui_state,
            100,
            18
        ));
    }

    #[test]
    fn tui_snapshot_revset_input() {
        let session = snapshot_session("");
        let mut input = RevsetInputState::new("trunk()", "@");
        input.toggle_field();
        let mode = Mode::RevsetInput(input);

        insta::assert_snapshot!(render_tui_text(&session, &mode, 100, 24));
    }

    #[test]
    fn tui_snapshot_operation_picker() {
        let session = snapshot_session("");
        let mode = Mode::OperationPicker(OperationPickerState::new(vec![
            crate::jj::JjOperationSummary {
                operation_id: "abc123def".to_owned(),
                time: "5 minutes ago".to_owned(),
                description: "snapshot working copy".to_owned(),
            },
            crate::jj::JjOperationSummary {
                operation_id: "456fed789".to_owned(),
                time: "2 hours ago".to_owned(),
                description: "commit working copy".to_owned(),
            },
        ]));

        insta::assert_snapshot!(render_tui_text(&session, &mode, 100, 24));
    }

    #[test]
    fn operation_picker_keeps_preview_and_hints_visible_at_watch_heights() {
        let session = snapshot_session("");
        let long_id = "abcdef0123456789".repeat(8);
        let mut picker = OperationPickerState::new(
            (0..40)
                .map(|index| crate::jj::JjOperationSummary {
                    operation_id: format!("op{index:03}"),
                    time: "moments ago".to_owned(),
                    description: format!("undo operation {long_id} with a long raw id"),
                })
                .collect(),
        );
        picker.preview =
            Some("will mark 5 caught up · 1 already viewed · 1 need re-review".to_owned());
        let mode = Mode::OperationPicker(picker);

        for height in [50, 30] {
            let rendered = render_tui_text(&session, &mode, 200, height);
            assert!(rendered.contains("will mark 5 caught up"));
            assert!(rendered.contains("j/k move · enter apply · esc cancel"));
        }
    }

    #[test]
    fn operation_picker_truncates_long_descriptions_to_one_row() {
        let session = snapshot_session("");
        let long_id = "abcdef0123456789".repeat(8);
        let mut picker = OperationPickerState::new(vec![crate::jj::JjOperationSummary {
            operation_id: "op000".to_owned(),
            time: "moments ago".to_owned(),
            description: format!("undo operation {long_id} with a long raw id"),
        }]);
        picker.preview = Some("will mark 1 caught up".to_owned());

        let rendered = render_tui_text(&session, &Mode::OperationPicker(picker), 100, 24);
        assert_eq!(rendered.matches("op000").count(), 1);
        assert!(rendered.contains('…'));
    }

    #[test]
    fn long_footer_notices_ellipsize_with_activity_cue() {
        let session = snapshot_session("");
        let tui_state = TuiState {
            notice: Some(UiNotice {
                level: UiNoticeLevel::Info,
                message: "repository changed — change abcdef updated · src/config.rs updated (+1 −0) · tests/basic.rs updated (+6 −0)".to_owned(),
            }),
            ..TuiState::default()
        };

        let rendered = render_tui_text_with_state(&session, &Mode::Normal, &tui_state, 80, 18);
        assert!(rendered.contains("… · ctrl-a for detail"));
    }

    #[test]
    fn tui_snapshot_jj_helpers_choose_and_confirm() {
        let session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1 +1 @@
-old
+new
"#,
        );
        let mut state = JjHelperState::for_session(&session);
        insta::assert_snapshot!(
            "tui_snapshot_jj_helpers_choose",
            render_tui_text(&session, &Mode::JjHelpers(state.clone()), 100, 24)
        );

        state.confirming = true;
        insta::assert_snapshot!(
            "tui_snapshot_jj_helpers_confirm",
            render_tui_text(&session, &Mode::JjHelpers(state), 100, 24)
        );
    }

    #[test]
    fn tui_snapshot_scrolled_diff_window() {
        let mut body = String::from(
            "diff --git a/big.txt b/big.txt\n--- a/big.txt\n+++ b/big.txt\n@@ -1,30 +1,30 @@\n",
        );
        for index in 1..=30 {
            if index == 5 {
                body.push_str("-removed line 5\n+line 5\n");
            } else {
                body.push_str(&format!(" line {index}\n"));
            }
        }
        let mut session = snapshot_session(&body);
        session.diff_scroll = 6;

        insta::assert_snapshot!(render_tui_text(&session, &Mode::Normal, 100, 16));
    }

    #[test]
    fn scrolled_diff_keeps_comment_lines_aligned() {
        let mut session = snapshot_session(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,3 @@
 one
-old
+new
 three
"#,
        );
        session.toggle_focus();
        session.move_diff_cursor(2);
        session.add_comment("note on new".into());
        session.comments[0].id = "pinned".to_owned();
        session.diff_scroll = 2;

        let unscrolled = render_tui_text(&session, &Mode::Normal, 100, 8);
        assert!(unscrolled.contains("↳ pinned"));

        // Scrolling past the commented row must not shift or duplicate the
        // remaining lines: line 4 of the full render becomes the first
        // diff line after scrolling by 4.
        session.diff_scroll = 4;
        let scrolled = render_tui_text(&session, &Mode::Normal, 100, 8);
        assert!(scrolled.contains("↳ pinned"));
        assert!(!scrolled.contains("a.txt  +1 -1"));
    }

    #[test]
    fn tui_snapshot_binary_and_large_placeholders() {
        let mut body = String::from(
            "diff --git a/logo.png b/logo.png\nBinary files a/logo.png and b/logo.png differ\ndiff --git a/huge.txt b/huge.txt\n--- a/huge.txt\n+++ b/huge.txt\n@@ -1,6 +1,6 @@\n",
        );
        for index in 1..=6 {
            body.push_str(&format!(" line {index}\n"));
        }
        let mut session = snapshot_session(&body);
        session.max_diff_lines = 5;
        // Select huge.txt to show the large-diff placeholder in the pane.
        let huge = session
            .files
            .iter()
            .position(|file| file.path == "huge.txt")
            .unwrap();
        session.jump_to_file(huge);

        insta::assert_snapshot!(render_tui_text(&session, &Mode::Normal, 100, 16));
    }

    #[test]
    fn tui_snapshot_agent_ordered_files() {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1 +1 @@
-old
+new
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-old
+new
"#,
        );
        session.apply_agent_overlay(&crate::agent::AgentOverlay {
            ordering: vec!["README.md".to_owned()],
            ..Default::default()
        });

        insta::assert_snapshot!(render_tui_text(&session, &Mode::Normal, 100, 16));
    }

    #[test]
    fn tui_snapshot_agent_flags_gutter_and_popup() {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,4 +1,5 @@
 fn main() {
-    old();
+    new();
+    extra();
 }
"#,
        );
        session.apply_agent_overlay(&crate::agent::AgentOverlay {
            flags: vec![crate::agent::AgentFlag {
                id: "flag-1".to_owned(),
                path: "src/app.rs".to_owned(),
                line: Some(2),
                reason: "unchecked call".to_owned(),
                priority: crate::agent::FlagPriority::Critical,
            }],
            ..Default::default()
        });

        insta::assert_snapshot!(
            "tui_snapshot_agent_flags_gutter",
            render_tui_text(&session, &Mode::Normal, 100, 18)
        );
        insta::assert_snapshot!(
            "tui_snapshot_agent_flags_popup",
            render_tui_text(
                &session,
                &Mode::FlagList(FlagListState::new(&session)),
                100,
                18
            )
        );
    }

    fn zen_snapshot_session() -> (ReviewSession, ZenState) {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,4 +1,5 @@
 fn main() {
-    old();
+    new();
+    extra();
 }
diff --git a/tests/app.rs b/tests/app.rs
--- a/tests/app.rs
+++ b/tests/app.rs
@@ -1 +1 @@
-check_old();
+check_new();
"#,
        );
        session.apply_agent_overlay(&crate::agent::AgentOverlay {
            chunks: vec![
                crate::agent::ReviewChunk {
                    id: "c1".to_owned(),
                    title: "core change".to_owned(),
                    importance: crate::agent::ChunkImportance::Spotlight,
                    change_id: None,
                    explanation: Some(
                        "main() now calls new() and adds extra() — the old single-call \
                         contract is gone, so every caller that relied on old() firing \
                         once must be re-checked."
                            .to_owned(),
                    ),
                    rationale: Some("start here".to_owned()),
                    artifacts: vec![crate::agent::Artifact {
                        title: "startup output".to_owned(),
                        kind: crate::agent::ArtifactKind::Output,
                        body: "$ cargo run\nnew: ready\nextra: ready".to_owned(),
                    }],
                    parts: vec![crate::agent::ChunkPart {
                        path: "src/app.rs".to_owned(),
                        start_line: Some(2),
                        end_line: Some(3),
                    }],
                },
                crate::agent::ReviewChunk {
                    id: "c2".to_owned(),
                    title: "test churn".to_owned(),
                    importance: crate::agent::ChunkImportance::Glance,
                    change_id: None,
                    explanation: None,
                    rationale: Some("mechanical rename in tests".to_owned()),
                    artifacts: Vec::new(),
                    parts: vec![crate::agent::ChunkPart {
                        path: "tests/app.rs".to_owned(),
                        start_line: Some(1),
                        end_line: Some(1),
                    }],
                },
            ],
            briefs: vec![crate::agent::ChangeBrief {
                change_id: "zzzzyyyy".to_owned(),
                summary: "Swaps the legacy old() startup call for new() and batches extra() \
                          alongside it, so both effects fire together."
                    .to_owned(),
                artifacts: Vec::new(),
            }],
            ..Default::default()
        });
        // A single-change stack: the opening chapter card carries the
        // change's full description and the agent's brief.
        let stack = vec![crate::jj::JjChangeSummary {
            change_id: "zzzzyyyy".to_owned(),
            bookmarks: "startup-fix".to_owned(),
            description: "feat: swap old() for new()\n\nold() fired a single effect; new() \
                          batches extra() alongside it\nso startup converges in one pass."
                .to_owned(),
        }];
        let mut zen = ZenState::new(&session, &stack).unwrap();
        session.file_pane_visible = false;
        // Land on the first spotlight stop (stops[0] is the chapter card).
        zen.index = 1;
        crate::tui::zen::jump_to_stop(&mut session, &zen.stops[1].clone());
        (session, zen)
    }

    fn fallback_zen_snapshot_session() -> (ReviewSession, ZenState) {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,4 +1,6 @@
 fn main() {
-    old();
+    new();
+    extra();
 }
+fn helper() {}
diff --git a/tests/app.rs b/tests/app.rs
--- a/tests/app.rs
+++ b/tests/app.rs
@@ -1 +1 @@
-check_old();
+check_new();
diff --git a/Cargo.toml b/Cargo.toml
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -1 +1,2 @@
 [dependencies]
+itertools = "1"
"#,
        );
        let zen = ZenState::new(&session, &[]).unwrap();
        session.file_pane_visible = false;
        (session, zen)
    }

    #[test]
    fn tui_snapshot_zen_chapter_card() {
        let (mut session, mut zen) = zen_snapshot_session();
        zen.index = 0;
        crate::tui::zen::jump_to_stop(&mut session, &zen.stops[0].clone());

        insta::assert_snapshot!(render_tui_text_with_zen(
            &session,
            &Mode::Normal,
            Some(&zen),
            100,
            24
        ));
    }

    #[test]
    fn tui_snapshot_zen_focus_card() {
        let (session, zen) = zen_snapshot_session();

        insta::assert_snapshot!(render_tui_text_with_zen(
            &session,
            &Mode::Normal,
            Some(&zen),
            100,
            24
        ));
    }

    #[test]
    fn tui_snapshot_zen_focus_card_long_header_truncates() {
        let (mut session, mut zen) = zen_snapshot_session();
        if let Some(ZenStop::Chunk(row)) = zen.stops.get_mut(1) {
            row.title =
                "very long generated client compatibility migration with unusually verbose title"
                    .to_owned();
            if let Some(part) = row.part.as_mut() {
                part.path =
                    "src/deeply/nested/generated/client/compatibility/transport/retry_policy.rs"
                        .to_owned();
            }
        }
        zen.index = 1;
        crate::tui::zen::jump_to_stop(&mut session, &zen.stops[1].clone());

        insta::assert_snapshot!(render_tui_text_with_zen(
            &session,
            &Mode::Normal,
            Some(&zen),
            80,
            24
        ));
    }

    #[test]
    fn tui_snapshot_fallback_zen_chapter_card() {
        let (mut session, mut zen) = fallback_zen_snapshot_session();
        zen.index = 0;
        crate::tui::zen::jump_to_stop(&mut session, &zen.stops[0].clone());

        insta::assert_snapshot!(render_tui_text_with_zen(
            &session,
            &Mode::Normal,
            Some(&zen),
            100,
            24
        ));
    }

    #[test]
    fn tui_snapshot_fallback_zen_stop() {
        let (mut session, mut zen) = fallback_zen_snapshot_session();
        zen.index = 1;
        crate::tui::zen::jump_to_stop(&mut session, &zen.stops[1].clone());

        insta::assert_snapshot!(render_tui_text_with_zen(
            &session,
            &Mode::Normal,
            Some(&zen),
            100,
            24
        ));
    }

    #[test]
    fn tui_snapshot_zen_chapter_card_collapsed_description() {
        let (mut session, mut zen) = zen_snapshot_session();
        zen.index = 0;
        zen.chapter_description_collapsed = true;
        crate::tui::zen::jump_to_stop(&mut session, &zen.stops[0].clone());

        insta::assert_snapshot!(render_tui_text_with_zen(
            &session,
            &Mode::Normal,
            Some(&zen),
            100,
            24
        ));
    }

    #[test]
    fn tui_snapshot_zen_artifact_viewer() {
        let (session, mut zen) = zen_snapshot_session();
        zen.phase = crate::tui::zen::ZenPhase::Artifact {
            index: 0,
            scroll: 0,
        };

        insta::assert_snapshot!(render_tui_text_with_zen(
            &session,
            &Mode::Normal,
            Some(&zen),
            100,
            24
        ));
    }

    #[test]
    fn tui_snapshot_zen_reading_panel() {
        let (session, mut zen) = zen_snapshot_session();
        zen.phase = crate::tui::zen::ZenPhase::Reading;

        insta::assert_snapshot!(render_tui_text_with_zen(
            &session,
            &Mode::Normal,
            Some(&zen),
            100,
            20
        ));
    }

    #[test]
    fn tui_snapshot_zen_glance_board() {
        let (session, mut zen) = zen_snapshot_session();
        zen.phase = crate::tui::zen::ZenPhase::Glance;

        insta::assert_snapshot!(render_tui_text_with_zen(
            &session,
            &Mode::Normal,
            Some(&zen),
            100,
            18
        ));
    }

    #[test]
    fn measured_content_width_tracks_excerpt_but_stays_readable() {
        assert_eq!(measured_content_width(200, 48), 72);
        assert_eq!(measured_content_width(200, 88), 88);
        assert_eq!(measured_content_width(200, 140), 100);
        assert_eq!(measured_content_width(80, 140), 80);
    }

    #[test]
    fn chunk_row_location_names_the_anchored_change() {
        let mut row = crate::tui::chunks::WalkthroughRow {
            source_id: "c1".to_owned(),
            title: "stop".to_owned(),
            importance: crate::agent::ChunkImportance::Spotlight,
            change_id: None,
            rationale: None,
            explanation: None,
            artifacts: Vec::new(),
            part: Some(crate::agent::ChunkPart {
                path: "src/app.rs".to_owned(),
                start_line: Some(3),
                end_line: Some(9),
            }),
            part_position: None,
            invalid_reason: None,
        };
        assert_eq!(chunk_row_location_raw(&row), "src/app.rs:3-9");

        row.change_id = Some("xyzkwqrs".to_owned());
        assert_eq!(chunk_row_location_raw(&row), "[xyzkwqrs] src/app.rs:3-9");
    }

    #[test]
    fn truncation_helpers_are_width_aware() {
        assert_eq!(truncate_tail("abcdef", 4), "abc…");
        assert_eq!(
            truncate_middle("src/very/deep/path/file.rs:10-20", 16),
            "src/v…e.rs:10-20"
        );
        assert!(truncate_tail("界界界", 5).width() <= 5);
    }

    #[test]
    fn glance_rows_group_multi_part_chunks() {
        let row = |path: &str, pos| crate::tui::chunks::WalkthroughRow {
            source_id: "c1".to_owned(),
            title: "shared".to_owned(),
            importance: crate::agent::ChunkImportance::Glance,
            change_id: None,
            rationale: Some("one reason".to_owned()),
            explanation: None,
            artifacts: Vec::new(),
            part: Some(crate::agent::ChunkPart {
                path: path.to_owned(),
                start_line: Some(1),
                end_line: Some(2),
            }),
            part_position: Some(pos),
            invalid_reason: None,
        };
        let rows = vec![row("a.rs", (1, 2)), row("b.rs", (2, 2))];
        let groups = grouped_glance_rows(&rows);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].indices, vec![0, 1]);
        assert_eq!(groups[0].rows.len(), 2);
    }

    #[test]
    fn zen_excerpt_follows_a_wandering_cursor_and_reports_it() {
        let mut body = String::from(
            "diff --git a/big.txt b/big.txt\n--- a/big.txt\n+++ b/big.txt\n@@ -1,30 +1,30 @@\n",
        );
        for index in 1..=30 {
            body.push_str(&format!("+line {index}\n"));
        }
        let mut session = snapshot_session(&body);
        session.focus = Focus::Diff;
        let rows = session.diff_rows_for_selected_file().to_vec();
        let stop = crate::tui::chunks::WalkthroughRow {
            source_id: "c1".to_owned(),
            title: "stop".to_owned(),
            importance: crate::agent::ChunkImportance::Spotlight,
            change_id: None,
            rationale: None,
            explanation: None,
            artifacts: Vec::new(),
            part: Some(crate::agent::ChunkPart {
                path: "big.txt".to_owned(),
                start_line: Some(2),
                end_line: Some(3),
            }),
            part_position: None,
            invalid_reason: None,
        };

        // Cursor inside the stop range: the excerpt is the range window.
        let in_range = rows
            .iter()
            .position(|row| row.new_lineno == Some(2))
            .unwrap();
        let (indices, wandered, _) = zen_excerpt_indices(&rows, &stop, 5, Some(in_range));
        assert!(!wandered);
        assert!(indices.contains(&in_range));
        assert_eq!(rows[indices[0]].new_lineno, Some(1)); // 2 - context

        // Cursor far below the range: the excerpt slides to keep it visible.
        let far = rows
            .iter()
            .position(|row| row.new_lineno == Some(20))
            .unwrap();
        let (indices, wandered, _) = zen_excerpt_indices(&rows, &stop, 5, Some(far));
        assert!(wandered);
        assert!(indices.contains(&far));
        assert_eq!(indices.len(), 5);
        // Roughly centered on the cursor.
        assert_eq!(rows[indices[0]].new_lineno, Some(18));

        // No cursor (files pane focus): the stop range wins.
        let (indices, wandered, _) = zen_excerpt_indices(&rows, &stop, 5, None);
        assert!(!wandered);
        assert_eq!(rows[indices[0]].new_lineno, Some(1));

        // At tiny prose-first heights, do not spend the whole budget on
        // leading context: start at the target range and still report the
        // clipped range trailer.
        let stop = crate::tui::chunks::WalkthroughRow {
            part: Some(crate::agent::ChunkPart {
                path: "big.txt".to_owned(),
                start_line: Some(10),
                end_line: Some(20),
            }),
            ..stop
        };
        let (indices, wandered, clipped) = zen_excerpt_indices(&rows, &stop, 3, None);
        assert!(!wandered);
        assert_eq!(rows[indices[0]].new_lineno, Some(10));
        assert_eq!(indices.len(), 2);
        assert_eq!(clipped, Some((11, 20)));

        // Exact budget boundary: the trailer still gets a reserved row rather
        // than being pushed out by the last visible code line.
        let (indices, wandered, clipped) = zen_excerpt_indices(&rows, &stop, 11, None);
        assert!(!wandered);
        assert_eq!(indices.len(), 10);
        assert_eq!(clipped, Some((3, 20)));
    }

    #[test]
    fn zen_focus_dims_rows_outside_the_stop_range() {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,4 +1,5 @@
 fn main() {
-    old();
+    new();
+    extra();
 }
"#,
        );
        session.zen_focus = Some(crate::app::ZenFocus {
            path: "src/app.rs".to_owned(),
            lines: Some((2, 3)),
        });
        session.focus = Focus::Diff;
        let rows = session.diff_rows_for_selected_file().to_vec();
        let in_range = rows
            .iter()
            .position(|row| row.new_lineno == Some(2))
            .unwrap();
        let out_of_range = rows
            .iter()
            .position(|row| row.new_lineno == Some(4))
            .unwrap();
        let header = rows
            .iter()
            .position(|row| matches!(row.kind, DiffRowKind::FileHeader))
            .unwrap();
        session.diff_cursor = usize::MAX; // keep the cursor away from the probes

        assert!(!zen_row_dimmed(&session, &rows[in_range], in_range));
        assert!(zen_row_dimmed(&session, &rows[out_of_range], out_of_range));
        assert!(zen_row_dimmed(&session, &rows[header], header));
    }

    #[test]
    fn zen_focus_never_dims_the_cursor_row_or_other_files() {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,4 +1,5 @@
 fn main() {
-    old();
+    new();
+    extra();
 }
"#,
        );
        session.focus = Focus::Diff;
        let rows = session.diff_rows_for_selected_file().to_vec();
        let out_of_range = rows
            .iter()
            .position(|row| row.new_lineno == Some(4))
            .unwrap();

        // Cursor row stays readable even outside the frame.
        session.zen_focus = Some(crate::app::ZenFocus {
            path: "src/app.rs".to_owned(),
            lines: Some((2, 3)),
        });
        session.diff_cursor = out_of_range;
        assert!(!zen_row_dimmed(&session, &rows[out_of_range], out_of_range));

        // A frame pointing at a different file dims nothing here.
        session.zen_focus = Some(crate::app::ZenFocus {
            path: "other.rs".to_owned(),
            lines: Some((2, 3)),
        });
        session.diff_cursor = usize::MAX;
        assert!(!zen_row_dimmed(&session, &rows[out_of_range], out_of_range));

        // A whole-file frame (chunkless fallback) dims nothing.
        session.zen_focus = Some(crate::app::ZenFocus {
            path: "src/app.rs".to_owned(),
            lines: None,
        });
        assert!(!zen_row_dimmed(&session, &rows[out_of_range], out_of_range));
    }

    #[test]
    fn tui_snapshot_agent_draft_list() {
        let mut session = snapshot_session(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        );
        session.apply_agent_overlay(&crate::agent::AgentOverlay {
            drafts: vec![
                crate::agent::AgentDraft {
                    id: "draft-1".to_owned(),
                    path: "a.txt".to_owned(),
                    line: Some(1),
                    body: "consider a clearer name".to_owned(),
                    state: crate::agent::DraftState::Pending,
                    accepted_comment_id: None,
                },
                crate::agent::AgentDraft {
                    id: "draft-2".to_owned(),
                    path: "a.txt".to_owned(),
                    line: None,
                    body: "file-level: needs tests".to_owned(),
                    state: crate::agent::DraftState::Pending,
                    accepted_comment_id: None,
                },
            ],
            ..Default::default()
        });
        let mode = Mode::DraftList(DraftListState::new(&session));

        insta::assert_snapshot!(render_tui_text(&session, &mode, 100, 20));
    }

    #[test]
    fn tui_snapshot_empty_state() {
        let session = snapshot_session("");

        insta::assert_snapshot!(render_tui_text(&session, &Mode::Normal, 100, 14));
    }

    #[test]
    fn tui_snapshot_selected_and_range_styles() {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,4 +1,5 @@
 fn main() {
-    old();
+    new();
+    extra();
 }
"#,
        );
        session.toggle_focus();
        session.set_diff_range_selection(3, 4);

        insta::assert_snapshot!(render_tui_style_runs(&session, &Mode::Normal, 80, 14));
    }

    #[test]
    fn parses_syntax_style_specs() {
        let style = syntax_style_spec("yellow bold underline");

        assert_eq!(style.fg, Some(Color::Yellow));
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn parses_background_indexed_and_hex_style_specs() {
        let style = syntax_style_spec("bold on 28");
        assert_eq!(style.bg, Some(Color::Indexed(28)));
        assert!(style.add_modifier.contains(Modifier::BOLD));

        let style = syntax_style_spec("#a1b2c3 on green");
        assert_eq!(style.fg, Some(Color::Rgb(0xa1, 0xb2, 0xc3)));
        assert_eq!(style.bg, Some(Color::Green));

        assert_eq!(syntax_style_spec("22").fg, Some(Color::Indexed(22)));
    }

    #[test]
    fn line_bg_specs_read_bare_colors_as_backgrounds() {
        assert_eq!(spec_bg_color("22"), Some(Color::Indexed(22)));
        assert_eq!(spec_bg_color("on red"), Some(Color::Red));
        assert_eq!(spec_bg_color("bold"), None);
    }

    #[test]
    fn quantizes_hex_tokens_to_nearest_indexed_colors() {
        // Exact cube colors map to their cube index.
        assert_eq!(nearest_indexed(0, 0, 0), 16);
        assert_eq!(nearest_indexed(255, 255, 255), 231);
        assert_eq!(nearest_indexed(0, 95, 0), 22);
        // Near-grays prefer the grayscale ramp over the coarse cube.
        assert_eq!(nearest_indexed(0x12, 0x12, 0x12), 233);
        // Dark tints keep their hue instead of flattening to black/gray.
        assert_eq!(nearest_indexed(0x12, 0x26, 0x1e), 22);
        assert_eq!(nearest_indexed(0x30, 0x1b, 0x1f), 52);

        assert_eq!(quantize_spec("bold on #1a4a29"), "bold on 22");
        assert_eq!(quantize_spec("#3fb950"), "71");
        // Named and indexed tokens pass through untouched.
        assert_eq!(quantize_spec("green bold"), "green bold");
        assert_eq!(quantize_spec("28"), "28");
    }

    #[test]
    fn downgrade_diff_theme_rewrites_all_hex_entries() {
        let mut theme = crate::config::DiffThemeConfig::default();

        downgrade_diff_theme(&mut theme);

        for spec in [
            &theme.added_line_bg,
            &theme.removed_line_bg,
            &theme.added_word,
            &theme.removed_word,
            &theme.gutter_added,
            &theme.gutter_removed,
        ] {
            assert!(!spec.contains('#'), "hex survived downgrade: {spec}");
            assert!(
                syntax_style_spec(spec) != Style::default(),
                "spec parses: {spec}"
            );
        }
    }

    #[test]
    fn overlay_emphasis_splits_segments_at_range_boundaries() {
        let base = Style::default().fg(Color::Green);
        let emphasis = Style::default().bg(Color::Indexed(28));

        let segments = overlay_emphasis(
            vec![("let x".to_owned(), base), (" = 2;".to_owned(), base)],
            &[4..5, 8..9],
            emphasis,
        );

        let texts: Vec<&str> = segments.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(texts, ["let ", "x", " = ", "2", ";"]);
        assert_eq!(segments[1].1.bg, Some(Color::Indexed(28)));
        assert_eq!(segments[1].1.fg, Some(Color::Green));
        assert_eq!(segments[3].1.bg, Some(Color::Indexed(28)));
        assert_eq!(segments[0].1.bg, None);
    }

    #[test]
    fn tui_snapshot_diff_visual_cues() {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,4 +1,4 @@
 fn main() {
-    let count = 1;
+    let count = 2;
 }
"#,
        );
        session.diff_cues.gutter_bar = true;

        insta::assert_snapshot!(render_tui_style_runs(&session, &Mode::Normal, 80, 12));
    }

    #[test]
    fn tui_snapshot_hidden_file_pane() {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,3 +1,3 @@
 fn main() {
-    old();
+    new();
 }
"#,
        );
        session.toggle_file_pane();

        insta::assert_snapshot!(render_tui_style_runs(&session, &Mode::Normal, 80, 12));
    }

    #[test]
    fn tui_snapshot_side_by_side_view() {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,4 +1,5 @@
 fn main() {
-    old();
+    new();
+    extra();
 }
"#,
        );
        session.toggle_diff_view();
        session.toggle_file_pane();

        insta::assert_snapshot!(render_tui_style_runs(&session, &Mode::Normal, 130, 12));
    }

    #[test]
    fn tui_snapshot_side_by_side_narrow_fallback() {
        let mut session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,3 +1,3 @@
 fn main() {
-    old();
+    new();
 }
"#,
        );
        session.toggle_diff_view();

        insta::assert_snapshot!(render_tui_style_runs(&session, &Mode::Normal, 100, 12));
    }

    #[test]
    fn measured_unified_wrap_preserves_unicode_and_logical_owner() {
        let combining = "e\u{301}";
        let family = "👨‍👩‍👧‍👦";
        let long = format!("prefix 界{combining}{family} suffix {}", "tail ".repeat(8));
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        session.toggle_focus();
        let rows = session.diff_rows_for_selected_file();
        let owner = rows.iter().position(|row| row.text == long).unwrap();
        session.select_diff_row(owner);

        let layout = measured_diff_layout(&session, &rows, Rect::new(0, 0, 30, 20), false);
        let visual: Vec<_> = layout
            .lines
            .iter()
            .filter(|line| line.hit.contains(owner))
            .collect();
        let painted: Vec<_> = visual
            .iter()
            .map(|line| {
                materialize_diff_source(
                    &session,
                    &rows,
                    layout.line_number_width,
                    layout.effective_horizontal_scroll(&session),
                    &line.source,
                )
            })
            .collect();

        assert!(visual.len() >= 3);
        assert!(
            visual
                .iter()
                .all(|line| line.hit == DiffVisualHit::Full(owner))
        );
        assert!(painted.iter().skip(1).all(|line| {
            line.spans
                .first()
                .is_some_and(|span| span.style.bg == Some(Color::DarkGray))
        }));
        let rendered = painted
            .iter()
            .map(|line| spans_text(&line.spans))
            .collect::<String>();
        assert!(rendered.contains('界'));
        assert!(rendered.contains(combining));
        assert!(rendered.contains(family));
        assert!(painted.iter().all(|line| line.width() == 30));
    }

    #[test]
    fn nowrap_horizontal_scroll_keeps_unified_gutter_fixed() {
        let long = "0123456789abcdefghijklmnopqrstuvwxyz";
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        session.diff_cues.soft_wrap = false;
        let rows = session.diff_rows_for_selected_file();
        let owner = rows.iter().position(|row| row.text == long).unwrap();
        let inner = Rect::new(0, 0, 24, 10);
        let before_layout = measured_diff_layout(&session, &rows, inner, false);
        let before = before_layout
            .lines
            .iter()
            .find(|line| line.hit.contains(owner))
            .map(|line| {
                spans_text(
                    &materialize_diff_source(
                        &session,
                        &rows,
                        before_layout.line_number_width,
                        before_layout.effective_horizontal_scroll(&session),
                        &line.source,
                    )
                    .spans,
                )
            })
            .unwrap();

        let tui_state = TuiState::default();
        scroll_diff_horizontal_visual(&mut session, inner, 10, &tui_state);
        let after_layout = measured_diff_layout(&session, &rows, inner, false);
        let after = after_layout
            .lines
            .iter()
            .find(|line| line.hit.contains(owner))
            .map(|line| {
                spans_text(
                    &materialize_diff_source(
                        &session,
                        &rows,
                        after_layout.line_number_width,
                        after_layout.effective_horizontal_scroll(&session),
                        &line.source,
                    )
                    .spans,
                )
            })
            .unwrap();

        assert_eq!(&before[..8], &after[..8]);
        assert!(before[8..].starts_with("0123"));
        assert!(after[8..].starts_with("abcd"));
        assert_eq!(before.width(), 24);
        assert_eq!(after.width(), 24);
    }

    #[test]
    fn five_digit_line_numbers_preserve_measured_width_and_split_hits() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -10000 +10000 @@\n-old value\n+new value\n",
        );
        let rows = session.diff_rows_for_selected_file();
        let left = rows.iter().position(|row| row.text == "old value").unwrap();
        let right = rows.iter().position(|row| row.text == "new value").unwrap();
        let unified = measured_diff_layout(&session, &rows, Rect::new(0, 0, 40, 10), false);
        assert!(unified.lines.iter().all(|line| {
            materialize_diff_source(&session, &rows, unified.line_number_width, 0, &line.source)
                .width()
                == 40
        }));

        session.diff_cues.view = DiffViewModeConfig::SideBySide;
        session.diff_scroll = left as u16;
        let inner = Rect::new(4, 2, 120, 10);
        let split = measured_diff_layout(&session, &rows, inner, true);
        let pair = split
            .lines
            .iter()
            .find(|line| line.hit.contains(left) && line.hit.contains(right))
            .unwrap();
        let pair_row = split
            .lines
            .iter()
            .position(|line| line.hit.contains(left) && line.hit.contains(right))
            .unwrap()
            .saturating_sub(split.viewport_start(&session, inner.height as usize));
        assert_eq!(
            materialize_diff_source(&session, &rows, split.line_number_width, 0, &pair.source,)
                .width(),
            120
        );
        let tui_state = TuiState::default();
        assert_eq!(
            diff_row_at_point(&session, inner, inner.x + 58, pair_row, &tui_state),
            Some(left)
        );
        assert_eq!(
            diff_row_at_point(&session, inner, inner.x + 60, pair_row, &tui_state),
            Some(right)
        );
    }

    #[test]
    fn horizontal_clip_inside_wide_grapheme_preserves_display_offset() {
        let spans = vec![
            Span::styled("界", Style::default().fg(Color::Blue)),
            Span::styled("abc", Style::default().fg(Color::Green)),
        ];

        let clipped = clip_spans(&spans, 1, 3);

        assert_eq!(spans_text(&clipped), " ab");
        assert_eq!(clipped[1].style.fg, Some(Color::Green));
        assert_eq!(clipped.iter().map(Span::width).sum::<usize>(), 3);
    }

    #[test]
    fn split_pairs_align_to_taller_wrapped_side_and_keep_divider_fixed() {
        let removed = "removed ".repeat(24);
        let added = "short replacement";
        let session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n-{removed}\n+{added}\n trailing\n"
        ));
        let rows = session.diff_rows_for_selected_file();
        let left = rows.iter().position(|row| row.text == removed).unwrap();
        let right = rows.iter().position(|row| row.text == added).unwrap();
        let trailing = rows.iter().position(|row| row.text == "trailing").unwrap();
        let inner = Rect::new(0, 0, 120, 30);
        let layout = measured_diff_layout(&session, &rows, inner, true);
        let pair: Vec<_> = layout
            .lines
            .iter()
            .filter(|line| line.block_anchor == left && !line.is_comment)
            .collect();

        assert!(pair.len() > 2);
        assert!(pair[0].hit.contains(left) && pair[0].hit.contains(right));
        assert!(
            pair.iter()
                .skip(1)
                .all(|line| line.hit.contains(left) && !line.hit.contains(right))
        );
        assert!(pair.iter().all(|line| {
            materialize_diff_source(&session, &rows, layout.line_number_width, 0, &line.source)
                .width()
                == 120
        }));
        assert!(pair.iter().all(|line| {
            matches!(line.hit, DiffVisualHit::Split { divider: 59, .. })
                && materialize_diff_source(
                    &session,
                    &rows,
                    layout.line_number_width,
                    0,
                    &line.source,
                )
                .spans
                .iter()
                .any(|span| span.content.contains('│'))
        }));
        assert_eq!(pair[1].hit.row_at(100), None);
        let pair_end = layout
            .lines
            .iter()
            .rposition(|line| line.hit.contains(left))
            .unwrap();
        let trailing_start = layout
            .lines
            .iter()
            .position(|line| line.hit.contains(trailing))
            .unwrap();
        assert!(trailing_start > pair_end);
    }

    #[test]
    fn split_nowrap_scroll_keeps_each_gutter_and_divider_fixed() {
        let removed = "0123456789 removed content ".repeat(5);
        let added = "abcdefghij added content ".repeat(5);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-{removed}\n+{added}\n"
        ));
        session.diff_cues.view = DiffViewModeConfig::SideBySide;
        session.diff_cues.soft_wrap = false;
        let rows = session.diff_rows_for_selected_file();
        let left = rows.iter().position(|row| row.text == removed).unwrap();
        let inner = Rect::new(0, 0, 120, 20);
        let before_layout = measured_diff_layout(&session, &rows, inner, true);
        let before_line = before_layout
            .lines
            .iter()
            .find(|line| line.hit.contains(left))
            .unwrap();
        let before_painted = materialize_diff_source(
            &session,
            &rows,
            before_layout.line_number_width,
            before_layout.effective_horizontal_scroll(&session),
            &before_line.source,
        );
        let before_text = spans_text(&before_painted.spans);

        let tui_state = TuiState::default();
        scroll_diff_horizontal_visual(&mut session, inner, 10, &tui_state);
        let after_layout = measured_diff_layout(&session, &rows, inner, true);
        let after_line = after_layout
            .lines
            .iter()
            .find(|line| line.hit.contains(left))
            .unwrap();
        let after_painted = materialize_diff_source(
            &session,
            &rows,
            after_layout.line_number_width,
            after_layout.effective_horizontal_scroll(&session),
            &after_line.source,
        );
        let after_text = spans_text(&after_painted.spans);

        assert_eq!(&before_text[..8], &after_text[..8]);
        assert_ne!(&before_text[8..18], &after_text[8..18]);
        assert!(matches!(
            before_line.hit,
            DiffVisualHit::Split { divider: 59, .. }
        ));
        assert!(matches!(
            after_line.hit,
            DiffVisualHit::Split { divider: 59, .. }
        ));
        assert_eq!(
            before_painted.spans.iter().map(Span::width).sum::<usize>(),
            120
        );
        assert_eq!(
            after_painted.spans.iter().map(Span::width).sum::<usize>(),
            120
        );
    }

    #[test]
    fn unpaired_split_continuations_hit_the_nonempty_logical_row() {
        let long = "unpaired removed ".repeat(15);
        let session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,1 @@\n-short\n-{long}\n+replacement\n"
        ));
        let rows = session.diff_rows_for_selected_file();
        let owner = rows.iter().position(|row| row.text == long).unwrap();
        let layout = measured_diff_layout(&session, &rows, Rect::new(0, 0, 120, 20), true);
        let visual: Vec<_> = layout
            .lines
            .iter()
            .filter(|line| line.hit.contains(owner))
            .collect();

        assert!(visual.len() > 1);
        assert!(visual.iter().all(|line| line.hit.row_at(10) == Some(owner)));
        assert!(visual.iter().all(|line| line.hit.row_at(110).is_none()));
    }

    #[test]
    fn reflow_clamps_continuation_offset_to_the_same_logical_block() {
        let long = "resize me ".repeat(30);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        let owner = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == long)
            .unwrap();
        session.diff_scroll = owner as u16;
        session.diff_visual_offset = 100;
        let rows = session.diff_rows_for_selected_file();
        let wide = measured_diff_layout(&session, &rows, Rect::new(0, 0, 90, 10), false);
        let start = wide.viewport_start(&session, 10);

        let owner_start = wide
            .lines
            .iter()
            .position(|line| line.hit.contains(owner))
            .unwrap();
        assert!(owner_start >= start && owner_start < start + 10);
        assert_eq!(session.diff_scroll as usize, owner);
    }

    #[test]
    fn cursor_visibility_targets_start_of_viewport_tall_row() {
        let long = "very tall wrapped row ".repeat(40);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        session.toggle_focus();
        let owner = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == long)
            .unwrap();
        session.select_diff_row(owner);
        session.diff_scroll = owner as u16;
        session.diff_visual_offset = 5;
        let inner = Rect::new(0, 0, 28, 3);
        let tui_state = TuiState::default();

        ensure_diff_cursor_visible(&mut session, inner, &tui_state);

        assert_eq!(session.diff_scroll as usize, owner);
        assert_eq!(session.diff_visual_offset, 0);
    }

    #[test]
    fn ultra_narrow_diff_degrades_gutter_before_content() {
        let text = "界abc";
        let session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -10000 +10000 @@\n-old\n+{text}\n"
        ));
        let rows = session.diff_rows_for_selected_file();
        let owner = rows.iter().position(|row| row.text == text).unwrap();

        for width in 1..=10 {
            let layout = measured_diff_layout(&session, &rows, Rect::new(0, 0, width, 20), false);
            let painted = layout
                .lines
                .iter()
                .filter(|line| line.hit.contains(owner))
                .map(|line| {
                    materialize_diff_source(
                        &session,
                        &rows,
                        layout.line_number_width,
                        0,
                        &line.source,
                    )
                })
                .collect::<Vec<_>>();
            let visible = painted
                .iter()
                .map(|line| spans_text(&line.spans))
                .collect::<String>();
            let compact = visible
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>();
            assert!(compact.contains("界abc"), "width {width}: {visible:?}");
        }
    }

    #[test]
    fn split_viewport_resolves_added_owner_before_anchor_fallback() {
        let session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n-old one\n-old two\n+new one\n+new two\n",
        );
        let rows = session.diff_rows_for_selected_file();
        let added = rows.iter().position(|row| row.text == "new one").unwrap();
        let mut session = session;
        session.diff_scroll = added as u16;
        let layout = measured_diff_layout(&session, &rows, Rect::new(0, 0, 120, 2), true);
        let expected = layout
            .lines
            .iter()
            .position(|line| line.hit.contains(added))
            .unwrap();

        assert_eq!(layout.viewport_start(&session, 2), expected);
    }

    #[test]
    fn stale_horizontal_offset_is_clamped_after_reflow_or_refresh() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+short\n",
        );
        session.diff_cues.soft_wrap = false;
        session.diff_horizontal_scroll = 500;
        let rows = session.diff_rows_for_selected_file();
        let owner = rows.iter().position(|row| row.text == "short").unwrap();
        let inner = Rect::new(0, 0, 80, 10);

        assert_eq!(
            measured_diff_layout(&session, &rows, inner, false)
                .effective_horizontal_scroll(&session),
            0
        );
        let layout = measured_diff_layout(&session, &rows, inner, false);
        let rendered = layout
            .lines
            .iter()
            .find(|line| line.hit.contains(owner))
            .map(|line| {
                spans_text(
                    &materialize_diff_source(
                        &session,
                        &rows,
                        layout.line_number_width,
                        layout.effective_horizontal_scroll(&session),
                        &line.source,
                    )
                    .spans,
                )
            })
            .unwrap();
        assert!(rendered.contains("short"));

        let tui_state = TuiState::default();
        scroll_diff_horizontal_visual(&mut session, inner, 4, &tui_state);
        assert_eq!(session.diff_horizontal_scroll, 0);
    }

    #[test]
    fn reconcile_normalizes_stored_visual_offset_and_reuses_geometry() {
        let long = "cached geometry ".repeat(200);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        let owner = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == long)
            .unwrap();
        session.diff_scroll = owner as u16;
        session.diff_visual_offset = usize::MAX;
        let inner = Rect::new(0, 0, 40, 5);
        let tui_state = TuiState::default();
        let rows = session.diff_rows_for_selected_file();
        let first = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);

        reconcile_diff_viewport(&mut session, inner, false, &tui_state);
        let second = cached_diff_layout(&session, rows, inner, false, &tui_state);

        assert!(Rc::ptr_eq(&first, &second));
        assert_eq!(tui_state.diff_layout_cache.borrow().builds, 1);
        assert!(session.diff_visual_offset < first.lines.len());
        let window = materialize_diff_window(
            &session,
            &session.diff_rows_for_selected_file(),
            &first,
            0,
            5,
        );
        assert_eq!(window.len(), 5);
    }

    #[test]
    fn geometry_cache_invalidates_for_comment_geometry_but_not_cursor_style() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        );
        session.toggle_focus();
        let inner = Rect::new(0, 0, 40, 5);
        let tui_state = TuiState::default();
        let rows = session.diff_rows_for_selected_file();
        let first = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);

        session.move_diff_cursor(1);
        let cursor_only = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);
        assert!(Rc::ptr_eq(&first, &cursor_only));

        let before_hash = diff_annotation_hash(&session);
        session.add_comment("a comment long enough to wrap across rows".into());
        assert_eq!(session.comments.len(), 1);
        assert_ne!(before_hash, diff_annotation_hash(&session));
        let with_comment = cached_diff_layout(&session, rows, inner, false, &tui_state);
        assert!(!Rc::ptr_eq(&first, &with_comment));
        assert!(with_comment.lines.len() > first.lines.len());
    }

    #[test]
    fn detached_scroll_survives_reflow_while_visible_cursor_stays_visible() {
        let mut body = String::from(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,20 +1,20 @@\n",
        );
        for line in 1..=20 {
            body.push_str(&format!(" line {line} {}\n", "wide ".repeat(8)));
        }
        let mut session = snapshot_session(&body);
        session.toggle_focus();
        let tui_state = TuiState::default();
        let old_inner = Rect::new(0, 0, 30, 5);
        let new_inner = Rect::new(0, 0, 60, 5);

        session.diff_scroll = 0;
        session.diff_visual_offset = 0;
        session.diff_cursor = session
            .diff_rows_for_selected_file()
            .iter()
            .rposition(|row| row.anchor.is_some())
            .unwrap();
        assert!(!diff_cursor_is_visible(&session, old_inner, &tui_state));
        reconcile_diff_viewport(&mut session, new_inner, false, &tui_state);
        assert_eq!(session.diff_scroll, 0);

        session.diff_scroll = session.diff_cursor as u16;
        reconcile_diff_viewport(&mut session, old_inner, true, &tui_state);
        assert!(diff_cursor_is_visible(&session, old_inner, &tui_state));
        reconcile_diff_viewport(&mut session, new_inner, true, &tui_state);
        assert!(diff_cursor_is_visible(&session, new_inner, &tui_state));
    }

    #[test]
    fn comments_and_visual_scrolling_keep_logical_association() {
        let long = "wrapped source ".repeat(10);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        session.toggle_focus();
        let owner = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == long)
            .unwrap();
        session.select_diff_row(owner);
        session.add_comment(format!("comment {}", "detail ".repeat(12)));
        session.comments[0].id = "wrapped-comment".into();
        let inner = Rect::new(0, 0, 32, 5);
        let rows = session.diff_rows_for_selected_file();
        let layout = measured_diff_layout(&session, &rows, inner, false);
        let comment_lines: Vec<_> = layout
            .lines
            .iter()
            .filter(|line| {
                line.hit.contains(owner) && matches!(line.source, DiffVisualSource::Comment { .. })
            })
            .collect();
        assert!(!comment_lines.is_empty());

        session.diff_scroll = owner as u16;
        let tui_state = TuiState::default();
        scroll_diff_visual(&mut session, inner, 2, &tui_state);
        assert_eq!(session.diff_scroll as usize, owner);
        assert!(session.diff_visual_offset > 0);
        assert_eq!(
            diff_row_at_point(&session, inner, inner.x + 10, 0, &tui_state),
            Some(owner)
        );
    }

    #[test]
    fn visual_scroll_clamps_to_a_full_final_page() {
        let mut body = String::from(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,20 +1,20 @@\n",
        );
        for line in 1..=20 {
            body.push_str(&format!(" line {line}\n"));
        }
        let mut session = snapshot_session(&body);
        let inner = Rect::new(0, 0, 40, 6);
        let tui_state = TuiState::default();
        scroll_diff_visual(&mut session, inner, isize::MAX, &tui_state);
        let rows = session.diff_rows_for_selected_file();
        let layout = measured_diff_layout(&session, &rows, inner, false);
        let start = layout.viewport_start(&session, 6);

        assert_eq!(start, layout.lines.len().saturating_sub(6));
        assert_eq!(layout.lines.len().saturating_sub(start), 6);
    }

    #[test]
    fn tui_snapshot_wrapped_unified_and_split_diff() {
        let removed = "removed unicode 界e\u{301} 👨‍👩‍👧‍👦 ".repeat(5);
        let added = "replacement ".repeat(3);
        let mut session = snapshot_session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-{removed}\n+{added}\n"
        ));
        session.toggle_focus();
        insta::assert_snapshot!(
            "tui_snapshot_wrapped_unified_diff",
            render_tui_text(&session, &Mode::Normal, 76, 14)
        );
        session.toggle_diff_view();
        session.toggle_file_pane();
        insta::assert_snapshot!(
            "tui_snapshot_wrapped_split_diff",
            render_tui_text(&session, &Mode::Normal, 130, 14)
        );
    }

    #[test]
    fn tui_snapshot_view_options_popup() {
        let session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,2 +1,2 @@
 fn main() {
-}
+} // end
"#,
        );

        insta::assert_snapshot!(render_tui_style_runs(
            &session,
            &Mode::ViewOptions(ViewOptionsState::default()),
            80,
            18
        ));
    }

    #[test]
    fn syntax_span_style_uses_theme() {
        let theme = SyntaxThemeConfig {
            keyword: "red italic".to_owned(),
            ..SyntaxThemeConfig::default()
        };
        let span = SyntaxSpan {
            text: "fn".to_owned(),
            kind: Some(HighlightKind::Keyword),
        };

        let style = syntax_span_style(&span, false, false, &theme);

        assert_eq!(style.fg, Some(Color::Red));
        assert!(style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn diff_renders_multiple_comments_for_anchor() {
        let mut session = snapshot_session(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        );
        session.toggle_focus();
        let anchor = session.selected_line_anchor().unwrap();
        session.comments.push(Comment {
            id: "c1".to_owned(),
            path: Some(anchor.path().to_owned()),
            line: anchor.line(),
            end_line: None,
            anchor: Some(anchor.clone()),
            body: "first note".to_owned(),
            kind: None,
            action: None,
            state: crate::state::CommentState::default(),
            created_at: chrono::Utc::now(),
            ..Default::default()
        });
        session.comments.push(Comment {
            id: "c2".to_owned(),
            path: Some(anchor.path().to_owned()),
            line: anchor.line(),
            end_line: None,
            anchor: Some(anchor),
            body: "second note".to_owned(),
            kind: None,
            action: None,
            state: crate::state::CommentState::default(),
            created_at: chrono::Utc::now(),
            ..Default::default()
        });

        let rendered = render_tui_text(&session, &Mode::Normal, 100, 16);

        assert!(rendered.contains("2   1 - old"));
        assert!(rendered.contains("↳ c1 [draft] first note"));
        assert!(rendered.contains("↳ c2 [draft] second note"));
    }

    #[test]
    fn target_picker_scrolls_selected_row_into_view() {
        assert_eq!(
            picker_visible_window(0, 20, 5),
            PickerVisibleWindow {
                start: 0,
                end: 4,
                hidden_above: 0,
                hidden_below: 16,
            }
        );
        let middle = picker_visible_window(6, 20, 5);
        assert!(middle.start <= 6 && 6 < middle.end);
        assert!(middle.hidden_above > 0);
        assert!(middle.hidden_below > 0);
        assert_eq!(
            picker_visible_window(19, 20, 5),
            PickerVisibleWindow {
                start: 16,
                end: 20,
                hidden_above: 16,
                hidden_below: 0,
            }
        );
        assert_eq!(
            picker_visible_window(2, 3, 5),
            PickerVisibleWindow {
                start: 0,
                end: 3,
                hidden_above: 0,
                hidden_below: 0,
            }
        );
    }
}
