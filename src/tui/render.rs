//! All drawing code: panes, popups, styles, and layout math.

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
    app::{DiffRow, DiffRowKind, Focus, ReviewSession, SplitRow, split_index_of, split_rows},
    config::DiffViewModeConfig,
    diff::DiffLineKind,
    file_tree::{FlatTreeRow, FlatTreeRowKind},
    jj::JjChangeSummary,
    state::Comment,
    syntax::{HighlightKind, SyntaxSpan, SyntaxThemeConfig},
};

use super::{
    ActivityListState, CommentInputTarget, Mode, TuiState, UiNotice, UiNoticeLevel,
    chooser::TargetChooserState,
    chunks::ChunkListState,
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
    tasks::TaskListState,
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
            draw_zen_focus(frame, body, session, zen.expect("checked above"));
            draw_footer(frame, layout.footer, session, mode, keymap, notice, zen);
        }
        Some(ZenPhase::Artifact { index, scroll }) => {
            let body = body_area(frame.area());
            let zen = zen.expect("checked above");
            draw_zen_focus(frame, body, session, zen);
            draw_zen_artifact(frame, body, zen, index, scroll);
            draw_footer(
                frame,
                layout.footer,
                session,
                mode,
                keymap,
                notice,
                Some(zen),
            );
        }
        Some(ZenPhase::Glance) => {
            let body = body_area(frame.area());
            draw_zen_glance(frame, body, session, zen.expect("checked above"));
            draw_footer(frame, layout.footer, session, mode, keymap, notice, zen);
        }
        _ => {
            if session.file_pane_visible {
                draw_files(frame, layout.files, session);
            }
            draw_diff(frame, layout.diff, session);
            draw_footer(frame, layout.footer, session, mode, keymap, notice, zen);

            // The zen reading panel is a layer under any popup: progress and
            // rationale stay visible while e.g. a comment is being written.
            if let Some(zen) = zen {
                draw_zen_panel(frame, frame.area(), session, zen);
            }
        }
    }

    match mode {
        Mode::TargetChooser(chooser) => draw_target_chooser_popup(frame, frame.area(), chooser),
        Mode::RevsetInput(input) => draw_revset_input_popup(frame, frame.area(), input),
        Mode::OperationPicker(picker) => draw_operation_picker_popup(frame, frame.area(), picker),
        Mode::JjHelpers(state) => draw_jj_helpers_popup(frame, frame.area(), state),
        Mode::FlagList(list) => draw_flag_list_popup(frame, frame.area(), list),
        Mode::TaskList(list) => draw_task_list_popup(frame, frame.area(), session, list),
        Mode::Activity(list) => draw_activity_popup(frame, frame.area(), tui_state, list),
        Mode::WalkthroughList(list) => {
            draw_walkthrough_list_popup(frame, frame.area(), session, list)
        }
        Mode::ChunkList(list) => draw_chunk_list_popup(frame, frame.area(), list),
        Mode::DraftList(list) => draw_draft_list_popup(frame, frame.area(), list),
        Mode::FileSearch(search) => draw_file_search_popup(frame, frame.area(), search),
        Mode::SymbolOutline(outline) => draw_symbol_outline_popup(frame, frame.area(), outline),
        Mode::CommentList(list) => draw_comment_list_popup(frame, frame.area(), session, list),
        Mode::ViewOptions(state) => draw_view_options_popup(frame, frame.area(), session, state),
        Mode::CommentInput { editor, target } => {
            draw_comment_popup(frame, frame.area(), session, editor, target, keymap)
        }
        Mode::Help => draw_help_popup(frame, frame.area(), keymap),
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
    let (mark, mark_style) = if file.changed_since_look {
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
    let style = if file.viewed || file.caught_up {
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

fn draw_diff(frame: &mut ratatui::Frame<'_>, area: Rect, session: &ReviewSession) {
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
    let lines = if split_active {
        split_diff_lines(session, &rows, inner)
    } else {
        unified_diff_lines(session, &rows, inner)
    };

    let mut title = diff_pane_title(session);
    if split_requested && !split_active {
        // Two unreadable half-panes help nobody: fall back to unified on
        // narrow terminals and say so in the title.
        title.push_str(" · unified (narrow)");
    }
    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

/// Minimum inner width (columns) for the side-by-side layout.
const MIN_SPLIT_WIDTH: u16 = 100;

/// Unified layout: one line per diff row plus attached comment summaries.
/// Lazy rendering: only construct styled lines for the visible window.
/// Rows before the scroll offset are counted (a row emits one line plus
/// one line per attached comment) but never built, so huge files cost
/// O(viewport) per frame instead of O(file).
fn unified_diff_lines(
    session: &ReviewSession,
    rows: &[DiffRow],
    inner: Rect,
) -> Vec<Line<'static>> {
    let scroll = session.diff_scroll as usize;
    let viewport_lines = inner.height as usize;
    let window_end = scroll.saturating_add(viewport_lines);
    let mut line_index = 0usize;
    let mut lines = Vec::new();
    let in_window = |line_index: usize| line_index >= scroll && line_index < window_end;
    for (index, row) in rows.iter().enumerate() {
        if line_index >= window_end {
            break;
        }
        let comments = row
            .anchor
            .as_ref()
            .map(|anchor| session.comments_for_diff_row_anchor_details(anchor))
            .unwrap_or_default();
        // Skip rows that end before the window without building any spans.
        let row_extent = 1 + comments.len();
        if line_index + row_extent <= scroll {
            line_index += row_extent;
            continue;
        }
        if in_window(line_index) {
            lines.push(unified_row_line(session, row, index, comments.len()));
        }
        line_index += 1;
        for comment in comments {
            if in_window(line_index) {
                lines.push(comment_summary_line(comment));
            }
            line_index += 1;
        }
    }
    lines
}

/// Side-by-side layout: removed/context cells on the left, added/context on
/// the right; headers and folds span the full width. The unified rows stay
/// the source of truth for the cursor, comments, and anchors
/// (docs/focused-diff-ux.md §4).
fn split_diff_lines(session: &ReviewSession, rows: &[DiffRow], inner: Rect) -> Vec<Line<'static>> {
    let split = split_rows(rows);
    if split.is_empty() {
        return Vec::new();
    }
    let viewport_lines = (inner.height as usize).max(1);
    // Session scroll/cursor positions are unified row indices; project them
    // into display-row space and keep the cursor inside the window.
    let cursor_display = split_index_of(&split, session.diff_cursor);
    let mut scroll = split_index_of(
        &split,
        (session.diff_scroll as usize).min(rows.len().saturating_sub(1)),
    );
    if cursor_display < scroll {
        scroll = cursor_display;
    } else if cursor_display >= scroll + viewport_lines {
        scroll = cursor_display + 1 - viewport_lines;
    }
    let window_end = scroll.saturating_add(viewport_lines);
    let in_window = |line_index: usize| line_index >= scroll && line_index < window_end;

    let cell_width = ((inner.width as usize).saturating_sub(1)) / 2;
    let divider = Span::styled("\u{2502}", Style::default().fg(Color::DarkGray));
    let mut lines = Vec::new();
    let mut line_index = 0usize;
    for row in &split {
        if line_index >= window_end {
            break;
        }
        let cells: Vec<usize> = match row {
            SplitRow::Full(index) => vec![*index],
            SplitRow::Pair { left, right } => {
                let mut cells: Vec<usize> = left.iter().chain(right.iter()).copied().collect();
                cells.dedup();
                cells
            }
        };
        let cell_comments: Vec<Vec<&Comment>> = cells
            .iter()
            .map(|index| {
                rows[*index]
                    .anchor
                    .as_ref()
                    .map(|anchor| session.comments_for_diff_row_anchor_details(anchor))
                    .unwrap_or_default()
            })
            .collect();
        let comment_lines: usize = cell_comments.iter().map(Vec::len).sum();
        if line_index + 1 + comment_lines <= scroll {
            line_index += 1 + comment_lines;
            continue;
        }

        if in_window(line_index) {
            let line = match row {
                SplitRow::Full(index) => {
                    unified_row_line(session, &rows[*index], *index, cell_comments[0].len())
                }
                SplitRow::Pair { left, right } => {
                    let mut spans = split_cell_spans(session, rows, *left, cell_width, true);
                    spans.push(divider.clone());
                    spans.extend(split_cell_spans(session, rows, *right, cell_width, false));
                    Line::from(spans)
                }
            };
            lines.push(line);
        }
        line_index += 1;
        for comment in cell_comments.into_iter().flatten() {
            if in_window(line_index) {
                lines.push(comment_summary_line(comment));
            }
            line_index += 1;
        }
    }
    lines
}

/// One side-by-side cell: the row rendered with its side-specific line
/// number, truncated and padded to the cell width. Empty cells pad blank.
fn split_cell_spans(
    session: &ReviewSession,
    rows: &[DiffRow],
    cell: Option<usize>,
    width: usize,
    is_left: bool,
) -> Vec<Span<'static>> {
    let Some(index) = cell else {
        return vec![Span::raw(" ".repeat(width))];
    };
    let row = &rows[index];
    let lineno = if is_left {
        row.old_lineno
    } else {
        row.new_lineno
    };
    let comment_count = row
        .anchor
        .as_ref()
        .map(|anchor| session.comments_for_diff_row_anchor_details(anchor).len())
        .unwrap_or(0);
    let mut spans = diff_line_cell_spans(session, row, index, lineno, comment_count);
    if zen_row_dimmed(session, row, index) {
        spans = dim_spans(spans);
    }
    fit_spans(spans, width)
}

/// Truncate spans to a display width and pad the remainder with spaces.
fn fit_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    use unicode_width::UnicodeWidthChar;

    let mut result = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let span_width = span.width();
        if used + span_width <= width {
            used += span_width;
            result.push(span);
            continue;
        }
        let mut text = String::new();
        for ch in span.content.chars() {
            let ch_width = ch.width().unwrap_or(0);
            if used + ch_width > width {
                break;
            }
            used += ch_width;
            text.push(ch);
        }
        if !text.is_empty() {
            result.push(Span::styled(text, span.style));
        }
        break;
    }
    if used < width {
        result.push(Span::raw(" ".repeat(width - used)));
    }
    result
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
        .map(|n| format!("{n:>4}"))
        .unwrap_or_else(|| "    ".to_owned());
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
    mode: &Mode,
    keymap: &KeyMap,
    notice: Option<&UiNotice>,
    zen: Option<&ZenState>,
) {
    let mode_text = match mode {
        Mode::Normal if zen.is_some() => {
            let zen = zen.expect("checked above");
            match zen.phase {
                ZenPhase::Focus => match zen.current() {
                    Some(ZenStop::Chapter(chapter)) => {
                        let mut text = format!(
                            "zen chapter {}/{} · n tours its {} stop(s)",
                            chapter.position.0, chapter.position.1, chapter.stop_count,
                        );
                        if !chapter.description_body().is_empty() {
                            text.push_str(if zen.chapter_description_collapsed {
                                " · d details"
                            } else {
                                " · d collapse/expand brief"
                            });
                        }
                        if !chapter.artifacts.is_empty() {
                            text.push_str(&format!(" · e {} artifact(s)", chapter.artifacts.len()));
                        }
                        text.push_str(" · p back · tab full diff · g glance · esc end");
                        text
                    }
                    current => {
                        let (current_stop, total) = zen.chunk_position();
                        let artifacts = current
                            .map(|stop| super::zen::stop_artifacts(stop).len())
                            .unwrap_or(0);
                        let artifact_hint = if artifacts > 0 {
                            format!(" · e {artifacts} artifact(s)")
                        } else {
                            String::new()
                        };
                        format!(
                            "zen {current_stop}/{total} · n next (marks viewed) · p back · j/k lines · . refocus · tab full diff · g glance{artifact_hint} · c comment · esc end",
                        )
                    }
                },
                ZenPhase::Reading => {
                    let (current, total) = zen.chunk_position();
                    format!(
                        "zen read {current}/{total} · n next · p back · . refocus · tab focus card · esc end · other keys as normal",
                    )
                }
                ZenPhase::Glance => format!(
                    "zen glance · {} item(s) · j/k move · enter jump · a mark all viewed & finish · p back · esc end",
                    zen.glance_rows.len(),
                ),
                ZenPhase::Artifact { index, .. } => {
                    let count = zen
                        .current()
                        .map(|stop| super::zen::stop_artifacts(stop).len())
                        .unwrap_or(0);
                    format!(
                        "zen artifact {}/{count} · j/k scroll · h/l switch · esc close",
                        (index + 1).min(count),
                    )
                }
            }
        }
        Mode::Normal if session.focus == Focus::Files => footer_line(files_footer_segments(
            session,
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
        Mode::Normal => footer_line(diff_footer_segments(
            session,
            keymap,
            &format!(
                "focus diff{}{}{}",
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
            ),
        )),
        Mode::CommentInput { target, .. } => format!(
            "{kind} comment · refresh paused · {newline} newline · {submit} save · {cancel} cancel",
            kind = match target {
                CommentInputTarget::New => "new",
                CommentInputTarget::Edit { .. } => "edit",
                CommentInputTarget::AcceptDraft { .. } => "accept draft",
            },
            newline = keymap.hint(Action::InsertNewline),
            submit = keymap.hint(Action::SubmitComment),
            cancel = keymap.hint(Action::CancelComment),
        ),
        Mode::TargetChooser(_) => {
            format!(
                "choose base/tip · refresh paused · type filter · tab side · {down}/{up} move · enter load · esc cancel",
                down = keymap.hint(Action::TargetPickerMoveDown),
                up = keymap.hint(Action::TargetPickerMoveUp),
            )
        }
        Mode::RevsetInput(_) => {
            "revset target · refresh paused · type revset · tab/↑/↓ switch field · enter load · esc cancel"
                .to_owned()
        }
        Mode::OperationPicker(_) => {
            "prior operation · refresh paused · ↑/↓ or n/e move · enter apply · esc cancel"
                .to_owned()
        }
        Mode::JjHelpers(state) => {
            if state.confirming {
                "confirm jj command · enter run · esc back".to_owned()
            } else {
                "jj helpers · j/k move · enter select · esc close".to_owned()
            }
        }
        Mode::FlagList(_) => "agent flags · j/k move · enter jump · esc close".to_owned(),
        Mode::TaskList(_) => {
            "review tasks · j/k move · enter jump · d cycle state · esc close".to_owned()
        }
        Mode::Activity(_) => "activity · j/k move · esc close".to_owned(),
        Mode::WalkthroughList(_) => {
            "walkthrough · j/k move · enter jump · J/K reorder · d delete · esc close".to_owned()
        }
        Mode::ChunkList(_) => "review chunks · j/k move · enter jump · esc close".to_owned(),
        Mode::DraftList(_) => {
            "agent drafts · j/k move · enter/a accept · e edit · x discard · esc close".to_owned()
        }
        Mode::FileSearch(_) => {
            format!(
                "file search · type filter · {down}/{up} move · enter open · esc cancel",
                down = keymap.hint(Action::TargetPickerMoveDown),
                up = keymap.hint(Action::TargetPickerMoveUp),
            )
        }
        Mode::SymbolOutline(_) => "changed symbols · j/k move · enter jump · esc cancel".to_owned(),
        Mode::CommentList(_) => {
            "comments · j/k move · enter jump · s state · a action · K kind · x delete · esc close"
                .to_owned()
        }
        Mode::ViewOptions(_) => {
            "view options · j/k move · space/enter toggle · esc close".to_owned()
        }
        Mode::Help => "help · any key to close".to_owned(),
    };
    let mut summary = session.summary_line();
    if session.target.is_symbolic() {
        summary.push_str(&format!(" · following {}", session.target.rev));
    }
    let mut lines = vec![Line::from(summary), Line::from(mode_text)];
    if let Some(notice) = notice {
        let (label, style) = match notice.level {
            UiNoticeLevel::Info => ("info", Style::default().fg(Color::Blue)),
            UiNoticeLevel::Error => ("error", Style::default().fg(Color::Red)),
        };
        lines[1] = Line::from(vec![
            Span::styled(format!("{label}: "), style.add_modifier(Modifier::BOLD)),
            Span::styled(notice.message.clone(), style),
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

fn hint_segments(keymap: &KeyMap, hints: &[FooterHint]) -> Vec<String> {
    hints.iter().map(|hint| hint.render(keymap)).collect()
}

fn files_footer_segments(
    session: &ReviewSession,
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
        FooterHint::new([Action::Help], "help"),
        FooterHint::new([Action::Quit], "quit"),
    ];
    let mut segments = vec![session.target.to_string(), focus_label.to_owned()];
    segments.extend(hint_segments(keymap, &hints));
    segments
}

fn diff_footer_segments(
    session: &ReviewSession,
    keymap: &KeyMap,
    focus_label: &str,
) -> Vec<String> {
    let hints = [
        FooterHint::new([Action::MoveDown, Action::MoveUp], "line"),
        FooterHint::new([Action::ScrollDown, Action::ScrollUp], "scroll"),
        FooterHint::new([Action::RangeComment], "range"),
        FooterHint::new([Action::MarkWalkthrough], "walkthrough"),
        FooterHint::new([Action::Comment], "comment"),
        FooterHint::new([Action::NextUnviewed, Action::PreviousUnviewed], "unviewed"),
        FooterHint::new([Action::ToggleFocus], "files"),
        FooterHint::new([Action::Help], "help"),
        FooterHint::new([Action::Quit], "quit"),
    ];
    let mut segments = vec![session.target.to_string(), focus_label.to_owned()];
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
/// everyday hints; this popup is the complete map. Two columns keep every
/// group visible on typical terminal heights.
fn draw_help_popup(frame: &mut ratatui::Frame<'_>, area: Rect, keymap: &KeyMap) {
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
        entry(&[Action::ViewOptions], "view options (visual cues)"),
        entry(&[Action::ToggleDiffView], "toggle side-by-side view"),
        entry(&[Action::ToggleLargeDiff], "expand/collapse huge diff"),
    ];
    let right = vec![
        section("zen keys"),
        literal("n/p", "next/back stop"),
        literal("tab", "toggle focus card / reading view"),
        literal("g", "open glance board"),
        literal("e", "open artifacts"),
        literal("d", "toggle chapter details / expand brief"),
        literal(".", "refocus current stop"),
        literal("a", "acknowledge glance items"),
        literal("esc", "leave zen / close artifact"),
        section("comments"),
        entry(&[Action::Comment], "comment at cursor"),
        entry(&[Action::RangeComment], "start/finish range comment"),
        entry(&[Action::EditComment], "edit comment"),
        entry(&[Action::DeleteComment], "delete comment"),
        entry(&[Action::CommentList], "comment list"),
        entry(&[Action::TaskList], "review tasks"),
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
        entry(&[Action::ChunkList], "agent review chunks"),
        entry(&[Action::Zen], "zen briefing (focus stops + glance)"),
        entry(&[Action::DraftList], "agent draft comments"),
        section("badges"),
        literal(
            "✓",
            "viewed · ◌ caught up · ~ done, changed since · ± changed since look",
        ),
    ];

    frame.render_widget(Block::default().borders(Borders::ALL).title("help"), popup);
    let inner = inner_bordered(popup);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner);
    frame.render_widget(Paragraph::new(left).wrap(Wrap { trim: false }), columns[0]);
    frame.render_widget(Paragraph::new(right).wrap(Wrap { trim: false }), columns[1]);
}

fn draw_comment_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    editor: &CommentEditor,
    target: &CommentInputTarget,
    keymap: &KeyMap,
) {
    let popup = centered_rect(70, 40, area);
    frame.render_widget(Clear, popup);
    let title = comment_popup_title(session, target, popup.width.saturating_sub(4) as usize);
    let hint = format!(
        "{} save · {} cancel",
        keymap.hint(Action::SubmitComment),
        keymap.hint(Action::CancelComment)
    );
    frame.render_widget(
        Paragraph::new(editor.text.clone())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .title_bottom(Line::from(Span::styled(
                        hint,
                        Style::default().fg(Color::DarkGray),
                    ))),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );

    let (line, col) = editor.line_col();
    let inner_x = popup.x.saturating_add(1);
    let inner_y = popup.y.saturating_add(1);
    frame.set_cursor_position((
        inner_x.saturating_add(col as u16),
        inner_y.saturating_add(line as u16),
    ));
}

fn comment_popup_title(
    session: &ReviewSession,
    target: &CommentInputTarget,
    max_width: usize,
) -> String {
    let kind = match target {
        CommentInputTarget::New => "comment",
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
    let location = target_anchor
        .map(
            |anchor| match (anchor.path(), anchor.line(), anchor.end_line()) {
                (path, Some(line), Some(end)) if end != line => format!("{path}:{line}-{end}"),
                (path, Some(line), _) => format!("{path}:{line}"),
                (path, None, _) => path.to_owned(),
            },
        )
        .unwrap_or_else(|| "unanchored".to_owned());
    truncate_middle(&format!("{kind} · {location}"), max_width)
}

fn draw_revset_input_popup(frame: &mut ratatui::Frame<'_>, area: Rect, input: &RevsetInputState) {
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
            "type revset · tab/↑/↓ switch field · enter load · esc cancel",
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

fn draw_jj_helpers_popup(frame: &mut ratatui::Frame<'_>, area: Rect, state: &JjHelperState) {
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
            "This rewrites history in your repo. enter run · esc back",
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
            "↑/↓ or j/k move · enter select · esc close",
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
) {
    let popup = centered_rect(50, 40, area);
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
        "j/k move · space/enter toggle · esc close",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("view options"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn draw_flag_list_popup(frame: &mut ratatui::Frame<'_>, area: Rect, list: &FlagListState) {
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
        "↑/↓ or j/k move · enter jump · esc close",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("agent flags"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn draw_chunk_list_popup(frame: &mut ratatui::Frame<'_>, area: Rect, list: &ChunkListState) {
    let popup = centered_rect(82, 60, area);
    frame.render_widget(Clear, popup);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 3usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window = picker_visible_window(list.selected, list.rows.len(), list_height);

    let mut lines = vec![Line::from(Span::styled(
        "Agent-suggested review units (can span or subdivide files)",
        Style::default().fg(Color::DarkGray),
    ))];
    if list.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no chunks",
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
            list.rows
                .iter()
                .enumerate()
                .skip(visible_window.start)
                .take(visible_window.end.saturating_sub(visible_window.start))
                .map(|(index, row)| {
                    let selected = index == list.selected;
                    let marker = if selected { "›" } else { " " };
                    let style = if selected {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    let position = row
                        .part_position
                        .map(|(part, total)| format!(" ({part}/{total})"))
                        .unwrap_or_default();
                    let location = chunk_row_location(row);
                    let rationale = row
                        .rationale
                        .as_deref()
                        .map(|rationale| format!(" — {rationale}"))
                        .unwrap_or_default();
                    let mut spans = vec![
                        Span::styled(format!("{marker} {}{position} ", row.title), style),
                        Span::styled(
                            format!("[{}] ", row.importance.label()),
                            if row.importance == crate::agent::ChunkImportance::Spotlight {
                                Style::default().fg(Color::Magenta)
                            } else {
                                Style::default().fg(Color::DarkGray)
                            },
                        ),
                        Span::styled(location, Style::default().fg(Color::Cyan)),
                        Span::styled(rationale, Style::default().fg(Color::DarkGray)),
                    ];
                    if let Some(reason) = &row.invalid_reason {
                        spans.push(Span::styled(" [invalid]", Style::default().fg(Color::Red)));
                        spans.push(Span::styled(
                            format!(" {reason}"),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
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
        "↑/↓ or j/k move · enter jump · esc close",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("review chunks"),
            )
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
                session.target.to_string()
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
        "enter/n next (marks viewed) · p back · . refocus · tab focus card · esc end · comment/flag/expand as normal",
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

fn chunk_row_location(row: &super::chunks::ChunkRow) -> String {
    chunk_row_location_raw(row)
}

fn chunk_row_location_width(row: &super::chunks::ChunkRow, max_width: usize) -> String {
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

fn chunk_row_location_raw(row: &super::chunks::ChunkRow) -> String {
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
) {
    match zen.current() {
        Some(ZenStop::Chapter(chapter)) => draw_zen_chapter(frame, area, session, zen, chapter),
        Some(ZenStop::Chunk(stop)) => draw_zen_stop(frame, area, session, zen, stop),
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
) {
    frame.render_widget(Clear, area);
    let (number, total) = chapter.position;
    let mut title = format!(" zen · chapter {number}/{total} ");
    if zen.has_glance() {
        title.push_str(&format!("· then {} at a glance ", zen.glance_rows.len()));
    }
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(title),
        area,
    );
    let inner = inner_bordered(area);

    // Where this chapter lives: its own change, or the walkthrough's target.
    let mut location = match &chapter.change_id {
        Some(change_id) => format!("change {change_id}"),
        None => session.target.to_string(),
    };
    if !chapter.bookmarks.is_empty() {
        location.push_str(&format!(" · {}", chapter.bookmarks));
    }

    let headline = if chapter.description.is_empty() {
        "(no description)".to_owned()
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
    let mut stops_hint = format!("n tours the {} stop(s) in this chapter", chapter.stop_count);
    if !chapter.artifacts.is_empty() {
        stops_hint.push_str(&format!(
            " · e opens {} artifact(s)",
            chapter.artifacts.len()
        ));
    }

    let mut body: Vec<Line<'static>> = vec![Line::from(Span::styled(
        headline,
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ))];
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
            for line in &description_body {
                body.push(Line::from(Span::styled(
                    (*line).to_owned(),
                    Style::default().fg(Color::Gray),
                )));
            }
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
    if zen.source == super::zen::ZenSource::Files {
        body.push(Line::from(Span::styled(
            "uncurated tour — derived from the diff; press @ to summon ACP/agent curation for intent/risk",
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        )));
    }
    for line in &chapter.derived_lines {
        body.push(Line::from(Span::styled(
            line.clone(),
            Style::default().fg(Color::Gray),
        )));
    }
    if inner.height < 8 {
        // Too small for the card layout: header only.
        body.insert(0, Line::from(location));
        frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), inner);
        return;
    }

    // Progress dots stay on the backdrop, like the stop cards.
    frame.render_widget(
        Paragraph::new(zen_progress_line(zen)),
        Rect { height: 1, ..inner },
    );

    let summary = chapter.summary.clone().unwrap_or_else(|| {
        "No agent brief for this change — the facts above are derived from the diff.".to_owned()
    });

    let max_card_width = inner.width.saturating_sub(4).max(20);
    let card_width = 100.min(max_card_width).max(50.min(max_card_width));
    let text_width = card_width.saturating_sub(4).max(20) as usize;
    let estimated: usize = summary
        .lines()
        .map(|line| line.chars().count().div_ceil(text_width).max(1))
        .sum();
    let full_summary_height = estimated as u16 + 1;
    let summary_height = if zen.chapter_brief_expanded {
        full_summary_height
            .min(inner.height.saturating_sub(6))
            .max(2)
    } else {
        full_summary_height.clamp(2, (inner.height / 2).max(3))
    };
    // Wrap-aware height for the top section so a long description body
    // gets the room it asked for (bounded by the screen).
    let estimated_body: usize = body
        .iter()
        .map(|line| line.width().div_ceil(text_width).max(1))
        .sum();
    let card_height =
        (estimated_body as u16 + summary_height + 2).min(inner.height.saturating_sub(2));

    let card = Rect {
        x: inner.x + inner.width.saturating_sub(card_width) / 2,
        y: inner.y + 1 + inner.height.saturating_sub(1).saturating_sub(card_height) / 2,
        width: card_width,
        height: card_height,
    };

    // The same one-cell drop shadow as the stop cards.
    let shadow = Rect {
        x: (card.x + 2).min(area.right().saturating_sub(1)),
        y: (card.y + 1).min(area.bottom().saturating_sub(1)),
        width: card.width.min(area.right().saturating_sub(card.x + 2)),
        height: card.height.min(area.bottom().saturating_sub(card.y + 1)),
    };
    if shadow.width > 0 && shadow.height > 0 {
        frame.render_widget(Clear, shadow);
        frame.render_widget(
            Block::default().style(Style::default().bg(Color::Black)),
            shadow,
        );
    }

    frame.render_widget(Clear, card);
    let chapter_location = chunk_location_label(&location, card.width.saturating_sub(22) as usize);
    let card_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Line::from(vec![
            Span::styled(
                format!(" chapter {number}/{total} "),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("· {chapter_location} "),
                Style::default().fg(Color::Cyan),
            ),
        ]));
    let card_inner = card_block.inner(card);
    frame.render_widget(card_block, card);

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(summary_height)])
        .split(card_inner);

    let mut chapter_body = body;
    let body_capacity = sections[0].height.saturating_sub(1) as usize;
    if body_capacity > 0 && chapter_body.len() > body_capacity {
        chapter_body.truncate(body_capacity);
        chapter_body.push(Line::from(Span::styled(
            "…",
            Style::default().fg(Color::DarkGray),
        )));
    }
    frame.render_widget(
        Paragraph::new(chapter_body)
            .block(Block::default().padding(Padding::horizontal(1)))
            .wrap(Wrap { trim: false }),
        sections[0],
    );
    let summary = if !zen.chapter_brief_expanded && full_summary_height > summary_height {
        format!(
            "{summary}\n… d expands the brief ({} more line(s))",
            full_summary_height - summary_height
        )
    } else {
        summary
    };
    frame.render_widget(
        Paragraph::new(summary)
            .style(Style::default().fg(Color::Gray))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(Color::DarkGray))
                    .padding(Padding::horizontal(1))
                    .title(Span::styled(
                        " what this change does ",
                        Style::default()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::BOLD),
                    )),
            )
            .wrap(Wrap { trim: false }),
        sections[1],
    );
}

fn chunk_location_label(location: &str, max_width: usize) -> String {
    truncate_middle(location, max_width)
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
    stop: &super::chunks::ChunkRow,
) {
    frame.render_widget(Clear, area);

    let (stop_number, stop_total) = zen.chunk_position();
    let mut title = format!(" zen · stop {stop_number}/{stop_total} ");
    if zen.has_glance() {
        title.push_str(&format!("· then {} at a glance ", zen.glance_rows.len()));
    }
    // The backdrop recedes (dim border) so the centered card pops.
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(title),
        area,
    );
    let inner = inner_bordered(area);
    if inner.height < 8 {
        // Too small for the card layout: fall back to the header only.
        frame.render_widget(
            Paragraph::new(zen_focus_header_lines(zen, stop)).wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }

    // Progress dots stay on the backdrop, top-left, like a slide counter.
    frame.render_widget(
        Paragraph::new(zen_focus_header_lines(zen, stop)),
        Rect {
            height: 2.min(inner.height),
            ..inner
        },
    );

    // Build the excerpt first: the card is sized to its content so the code
    // and the explanation sit together in one framed unit instead of
    // drifting apart across a tall terminal. When the cursor wanders off
    // the stop the excerpt follows it (a scrolling window), so line
    // navigation never walks out of view; `.` snaps back.
    let rows = session.diff_rows_for_selected_file();
    let max_excerpt = ((inner.height as usize).saturating_sub(8)).clamp(3, 24);
    let cursor = (session.focus == Focus::Diff).then_some(session.diff_cursor);
    let (indices, wandered) = zen_excerpt_indices(&rows, stop, max_excerpt, cursor);
    let mut excerpt: Vec<Line<'static>> = Vec::new();
    if indices.is_empty() {
        excerpt.push(Line::from(Span::styled(
            "  (no diff lines to excerpt — tab shows the full file)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    for index in indices {
        excerpt.push(unified_row_line(session, &rows[index], index, 0));
    }

    let (explanation, explanation_title) = if zen.source == super::zen::ZenSource::Files {
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

    // Card width: hug the widest excerpt line (plus breathing room), but
    // stay wide enough for prose and inside the backdrop.
    let widest = excerpt.iter().map(Line::width).max().unwrap_or(40);
    let max_card_width = inner.width.saturating_sub(4).max(20);
    let card_width =
        ((widest as u16).saturating_add(4)).clamp(50.min(max_card_width), max_card_width);

    let text_width = card_width.saturating_sub(4).max(20) as usize;
    let estimated: usize = explanation
        .lines()
        .map(|line| line.chars().count().div_ceil(text_width).max(1))
        .sum();
    // The explanation block carries its own top border (the divider), so
    // +1 covers it; clamp so prose can never crowd out the code.
    let explanation_height = (estimated as u16 + 1).clamp(2, (inner.height / 3).max(3));
    let card_height =
        (excerpt.len() as u16 + explanation_height + 2).min(inner.height.saturating_sub(2));

    // Center the card in the backdrop (below the dots row).
    let card = Rect {
        x: inner.x + inner.width.saturating_sub(card_width) / 2,
        y: inner.y + 1 + inner.height.saturating_sub(1).saturating_sub(card_height) / 2,
        width: card_width,
        height: card_height,
    };

    // A one-cell drop shadow sells the "popped up" effect.
    let shadow = Rect {
        x: (card.x + 2).min(area.right().saturating_sub(1)),
        y: (card.y + 1).min(area.bottom().saturating_sub(1)),
        width: card.width.min(area.right().saturating_sub(card.x + 2)),
        height: card.height.min(area.bottom().saturating_sub(card.y + 1)),
    };
    if shadow.width > 0 && shadow.height > 0 {
        frame.render_widget(Clear, shadow);
        frame.render_widget(
            Block::default().style(Style::default().bg(Color::Black)),
            shadow,
        );
    }

    frame.render_widget(Clear, card);
    let position = stop
        .part_position
        .map(|(part, total)| format!(" (part {part}/{total})"))
        .unwrap_or_default();
    let title_budget = card.width.saturating_sub(4) as usize;
    let location_title = chunk_row_location_width(stop, title_budget / 2);
    let stop_title = truncate_tail(
        &stop.title,
        title_budget.saturating_sub(location_title.width() + position.width() + 8),
    );
    let card_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if wandered {
            Color::DarkGray
        } else {
            Color::Yellow
        }))
        .title(Line::from({
            let mut spans = vec![
                Span::styled(
                    format!(" {stop_title}{position} "),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("· {location_title} "),
                    Style::default().fg(Color::Cyan),
                ),
            ];
            if !stop.artifacts.is_empty() {
                spans.push(Span::styled(
                    format!("· e {} artifact(s) ", stop.artifacts.len()),
                    Style::default().fg(Color::Magenta),
                ));
            }
            if wandered {
                spans.push(Span::styled(
                    "· off the stop — . refocuses ",
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            spans
        }));
    let card_inner = card_block.inner(card);
    frame.render_widget(card_block, card);

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(explanation_height)])
        .split(card_inner);

    frame.render_widget(Paragraph::new(excerpt), sections[0]);
    let mut explanation_lines = vec![Line::from(explanation)];
    if zen.source == super::zen::ZenSource::Files
        && let Some(mechanics) = &stop.explanation
    {
        explanation_lines.push(Line::from(Span::styled(
            mechanics.clone(),
            Style::default().fg(Color::DarkGray),
        )));
    }
    if let Some(sibling) = sibling_parts_line(zen, stop, card_width.saturating_sub(6) as usize) {
        explanation_lines.push(Line::from(Span::styled(
            sibling,
            Style::default().fg(Color::DarkGray),
        )));
    }
    frame.render_widget(
        Paragraph::new(explanation_lines)
            .style(Style::default().fg(Color::Gray))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(Color::DarkGray))
                    .padding(Padding::horizontal(1))
                    .title(Span::styled(
                        explanation_title,
                        Style::default()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::BOLD),
                    )),
            )
            .wrap(Wrap { trim: false }),
        sections[1],
    );
}

/// Header lines for the focus card backdrop: the progress dots.
fn zen_focus_header_lines(zen: &ZenState, stop: &super::chunks::ChunkRow) -> Vec<Line<'static>> {
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
    stop: &super::chunks::ChunkRow,
    max_width: usize,
) -> Option<String> {
    let parts = zen
        .stops
        .iter()
        .filter_map(|candidate| match candidate {
            ZenStop::Chunk(row) if row.chunk_id == stop.chunk_id && row.part != stop.part => {
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
    stop: &super::chunks::ChunkRow,
    max_rows: usize,
    cursor: Option<usize>,
) -> (Vec<usize>, bool) {
    const CONTEXT: usize = 2;
    let range = stop.part.as_ref().and_then(|part| {
        part.start_line
            .map(|start| (start, part.end_line.unwrap_or(start)))
    });
    let mut indices: Vec<usize> = match range {
        Some((start, end)) => {
            let lo = start.saturating_sub(CONTEXT);
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
    indices.truncate(max_rows.max(1));

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
            let max_rows = max_rows.max(1);
            let start = position.saturating_sub(max_rows / 2);
            let end = (start + max_rows).min(all.len());
            let start = end.saturating_sub(max_rows);
            return (all[start..end].to_vec(), true);
        }
    }
    (indices, false)
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
            format!("· {}/{} (h/l switch) ", index + 1, artifacts.len()),
            Style::default().fg(Color::Cyan),
        ));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Magenta))
        .title(Line::from(title_spans))
        .title_bottom(Line::from(Span::styled(
            " j/k scroll · esc close ",
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
) {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(" zen · at a glance "),
        area,
    );
    let inner = inner_bordered(area);

    let glance_groups = grouped_glance_rows(&zen.glance_rows);
    let selected_group = glance_groups
        .iter()
        .position(|group| group.indices.contains(&zen.glance_selected))
        .unwrap_or(0);
    let fixed_lines = 4usize;
    let list_height = (inner.height as usize).saturating_sub(fixed_lines).max(1);
    let window = picker_visible_window(selected_group, glance_groups.len(), list_height);

    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{} spotlight stop(s) toured. ", zen.chunk_stop_count()),
                Style::default().fg(Color::Gray),
            ),
            Span::styled(
                format!(
                    "{} glance group(s) below ({} item(s)) — skim, then `a` marks them all viewed.",
                    glance_groups.len(),
                    zen.glance_rows.len()
                ),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
    ];
    if window.hidden_above > 0 {
        lines.push(Line::from(Span::styled(
            format!("  ↑ {} more", window.hidden_above),
            Style::default().fg(Color::DarkGray),
        )));
    }
    for (group_index, group) in glance_groups
        .iter()
        .enumerate()
        .skip(window.start)
        .take(window.end.saturating_sub(window.start))
    {
        let row = group.rows[0];
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
        let stats = row
            .part
            .as_ref()
            .and_then(|part| session.files.iter().find(|file| file.path == part.path))
            .map(|file| format!(" +{} -{}", file.additions, file.deletions))
            .unwrap_or_default();
        let rationale = row
            .rationale
            .as_deref()
            .map(|rationale| format!(" — {rationale}"))
            .unwrap_or_default();
        let locations = group
            .rows
            .iter()
            .map(|row| chunk_row_location_width(row, 28))
            .collect::<Vec<_>>()
            .join(" · ");
        let title = if group.rows.len() == 1
            && row.part.as_ref().is_some_and(|part| part.path == row.title)
        {
            String::new()
        } else {
            format!("{} ", row.title)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker} "), style),
            Span::styled(format!("{check} "), Style::default().fg(Color::Green)),
            Span::styled(title, style),
            Span::styled(locations, Style::default().fg(Color::Cyan)),
            Span::styled(stats, Style::default().fg(Color::Magenta)),
            Span::styled(rationale, Style::default().fg(Color::DarkGray)),
        ]));
    }
    if window.hidden_below > 0 {
        lines.push(Line::from(Span::styled(
            format!("  ↓ {} more", window.hidden_below),
            Style::default().fg(Color::DarkGray),
        )));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

struct GlanceGroup<'a> {
    indices: Vec<usize>,
    rows: Vec<&'a super::chunks::ChunkRow>,
}

fn grouped_glance_rows(rows: &[super::chunks::ChunkRow]) -> Vec<GlanceGroup<'_>> {
    let mut groups: Vec<GlanceGroup<'_>> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if row.part_position.is_some()
            && let Some(group) = groups.iter_mut().find(|group| {
                group
                    .rows
                    .first()
                    .is_some_and(|first| first.chunk_id == row.chunk_id)
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

fn draw_draft_list_popup(frame: &mut ratatui::Frame<'_>, area: Rect, list: &DraftListState) {
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
        "↑/↓ or j/k move · enter/a accept · e edit then accept · x discard · esc close",
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
                    Line::from(vec![
                        Span::styled(format!("{marker} {:<14}", operation.operation_id), style),
                        Span::styled(
                            format!("{:<18}", operation.time),
                            Style::default().fg(Color::Cyan),
                        ),
                        Span::styled(description.to_owned(), style),
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
        "↑/↓ or n/e move · enter apply · esc cancel",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("prior operations"),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn draw_file_search_popup(frame: &mut ratatui::Frame<'_>, area: Rect, search: &FileSearchState) {
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
        "type fuzzy filter · ↑/↓ or ctrl-j/ctrl-k move · enter open file · esc cancel",
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
        "↑/↓ or j/k move · enter jump · esc cancel",
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

fn draw_comment_list_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    list: &CommentListState,
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
                    let location = match (comment.line, comment.end_line) {
                        (Some(line), Some(end_line)) => {
                            format!("{}:{line}-{end_line}", comment.path)
                        }
                        (Some(line), None) => format!("{}:{line}", comment.path),
                        _ => comment.path.clone(),
                    };
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
        "↑/↓ or j/k move · enter jump · s state · a action · K kind · x delete · esc close",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("comments"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn draw_task_list_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    list: &TaskListState,
) {
    let popup = centered_rect(82, 60, area);
    frame.render_widget(Clear, popup);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 2usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window = picker_visible_window(list.selected, list.comment_ids.len(), list_height);

    let mut lines = Vec::new();
    if list.comment_ids.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no review tasks",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(Color::DarkGray),
            )));
        }
        for (index, id) in list
            .comment_ids
            .iter()
            .enumerate()
            .skip(visible_window.start)
            .take(visible_window.end.saturating_sub(visible_window.start))
        {
            let Some(comment) = session.comments.iter().find(|comment| &comment.id == id) else {
                continue;
            };
            let selected = index == list.selected;
            let marker = if selected { "›" } else { " " };
            let style = if selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            let location = match (comment.line, comment.end_line) {
                (Some(line), Some(end_line)) => format!("{}:{line}-{end_line}", comment.path),
                (Some(line), None) => format!("{}:{line}", comment.path),
                _ => comment.path.clone(),
            };
            let summary = comment
                .body
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("(empty comment)")
                .trim()
                .to_owned();
            let mut spans = vec![
                Span::styled(format!("{marker} "), style),
                Span::styled(format!("{location} "), Style::default().fg(Color::Cyan)),
            ];
            spans.extend(comment_badge_spans(comment));
            spans.extend([
                Span::styled(
                    format!("[{}] ", comment.state.label()),
                    comment_state_style(comment.state),
                ),
                Span::styled(summary, style),
            ]);
            lines.push(Line::from(spans));
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
        "↑/↓ or j/k move · enter jump · d cycle state · esc close",
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Review tasks ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, popup);
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
            let time = event
                .timestamp
                .with_timezone(&chrono::Local)
                .format("%H:%M:%S")
                .to_string();
            let message = if event.count > 1 {
                format!("{} {}×", event.message, event.count)
            } else {
                event.message.clone()
            };
            ListItem::new(Line::from(vec![
                Span::styled(time, Style::default().fg(Color::DarkGray)),
                Span::raw("  "),
                Span::raw(message),
            ]))
        })
        .collect();
    let mut list_state = ListState::default();
    if !items.is_empty() {
        list_state.select(Some(state.selected.min(items.len() - 1)));
    }
    let list = List::new(items)
        .block(block)
        .highlight_symbol("› ")
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, popup, &mut list_state);
}

fn draw_walkthrough_list_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    list: &WalkthroughListState,
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
        "j/k move · enter jump · J/K reorder · d delete · esc close",
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
        "type fuzzy filter · tab base/tip · ↑/↓ or ctrl-j/ctrl-k move · enter use selected · esc cancel",
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
            editor: CommentEditor {
                text: "Looks good\nexcept this line".to_owned(),
                cursor: "Looks good\nexcept".len(),
            },
            target: CommentInputTarget::New,
        };

        insta::assert_snapshot!(render_tui_text(&session, &mode, 100, 24));
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
    fn tui_snapshot_task_list() {
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
        session.add_comment("Please add a regression test".into());
        session.comments[1].id = "test-task".to_owned();
        session.comments[1].action = Some(crate::state::ActionIntent::Test);
        session.comments[1].kind = Some(crate::state::CommentKind::Issue);
        let mode = Mode::TaskList(TaskListState::new(&session));

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

        let unscrolled = render_tui_text(&session, &Mode::Normal, 100, 16);
        assert!(unscrolled.contains("↳ pinned"));

        // Scrolling past the commented row must not shift or duplicate the
        // remaining lines: line 4 of the full render becomes the first
        // diff line after scrolling by 4.
        session.diff_scroll = 4;
        let scrolled = render_tui_text(&session, &Mode::Normal, 100, 16);
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

    #[test]
    fn tui_snapshot_chunk_list() {
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
            chunks: vec![crate::agent::ReviewChunk {
                id: "c1".to_owned(),
                title: "core change".to_owned(),
                importance: crate::agent::ChunkImportance::Spotlight,
                change_id: None,
                explanation: None,
                rationale: Some("start here".to_owned()),
                artifacts: Vec::new(),
                parts: vec![
                    crate::agent::ChunkPart {
                        path: "src/app.rs".to_owned(),
                        start_line: Some(2),
                        end_line: Some(3),
                    },
                    crate::agent::ChunkPart {
                        path: "src/app.rs".to_owned(),
                        start_line: Some(4),
                        end_line: None,
                    },
                ],
            }],
            ..Default::default()
        });
        let mode = Mode::ChunkList(ChunkListState::new(&session));

        insta::assert_snapshot!(render_tui_text(&session, &mode, 100, 20));
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
    fn chunk_row_location_names_the_anchored_change() {
        let mut row = crate::tui::chunks::ChunkRow {
            chunk_id: "c1".to_owned(),
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
        assert_eq!(chunk_row_location(&row), "src/app.rs:3-9");

        row.change_id = Some("xyzkwqrs".to_owned());
        assert_eq!(chunk_row_location(&row), "[xyzkwqrs] src/app.rs:3-9");
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
        let row = |path: &str, pos| crate::tui::chunks::ChunkRow {
            chunk_id: "c1".to_owned(),
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
        let stop = crate::tui::chunks::ChunkRow {
            chunk_id: "c1".to_owned(),
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
        let (indices, wandered) = zen_excerpt_indices(&rows, &stop, 5, Some(in_range));
        assert!(!wandered);
        assert!(indices.contains(&in_range));
        assert_eq!(rows[indices[0]].new_lineno, Some(1)); // 2 - context

        // Cursor far below the range: the excerpt slides to keep it visible.
        let far = rows
            .iter()
            .position(|row| row.new_lineno == Some(20))
            .unwrap();
        let (indices, wandered) = zen_excerpt_indices(&rows, &stop, 5, Some(far));
        assert!(wandered);
        assert!(indices.contains(&far));
        assert_eq!(indices.len(), 5);
        // Roughly centered on the cursor.
        assert_eq!(rows[indices[0]].new_lineno, Some(18));

        // No cursor (files pane focus): the stop range wins.
        let (indices, wandered) = zen_excerpt_indices(&rows, &stop, 5, None);
        assert!(!wandered);
        assert_eq!(rows[indices[0]].new_lineno, Some(1));
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
            path: anchor.path().to_owned(),
            line: anchor.line(),
            end_line: None,
            anchor: Some(anchor.clone()),
            body: "first note".to_owned(),
            kind: None,
            action: None,
            state: crate::state::CommentState::default(),
            created_at: chrono::Utc::now(),
        });
        session.comments.push(Comment {
            id: "c2".to_owned(),
            path: anchor.path().to_owned(),
            line: anchor.line(),
            end_line: None,
            anchor: Some(anchor),
            body: "second note".to_owned(),
            kind: None,
            action: None,
            state: crate::state::CommentState::default(),
            created_at: chrono::Utc::now(),
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
