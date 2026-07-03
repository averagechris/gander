//! All drawing code: panes, popups, styles, and layout math.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    app::{DiffRow, DiffRowKind, Focus, ReviewSession},
    diff::DiffLineKind,
    file_tree::{FlatTreeRow, FlatTreeRowKind},
    jj::JjChangeSummary,
    state::Comment,
    syntax::{HighlightKind, SyntaxSpan, SyntaxThemeConfig},
};

use super::{
    CommentInputTarget, Mode, UiNotice, UiNoticeLevel,
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
};

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
    notice: Option<&UiNotice>,
) {
    let layout = ui_layout(frame.area());

    draw_files(frame, layout.files, session);
    draw_diff(frame, layout.diff, session);
    draw_footer(frame, layout.footer, session, mode, keymap, notice);

    match mode {
        Mode::TargetChooser(chooser) => draw_target_chooser_popup(frame, frame.area(), chooser),
        Mode::RevsetInput(input) => draw_revset_input_popup(frame, frame.area(), input),
        Mode::OperationPicker(picker) => draw_operation_picker_popup(frame, frame.area(), picker),
        Mode::JjHelpers(state) => draw_jj_helpers_popup(frame, frame.area(), state),
        Mode::FlagList(list) => draw_flag_list_popup(frame, frame.area(), list),
        Mode::ChunkList(list) => draw_chunk_list_popup(frame, frame.area(), list),
        Mode::DraftList(list) => draw_draft_list_popup(frame, frame.area(), list),
        Mode::FileSearch(search) => draw_file_search_popup(frame, frame.area(), search),
        Mode::SymbolOutline(outline) => draw_symbol_outline_popup(frame, frame.area(), outline),
        Mode::CommentList(list) => draw_comment_list_popup(frame, frame.area(), session, list),
        Mode::CommentInput { editor, .. } => draw_comment_popup(frame, frame.area(), editor),
        Mode::Help => draw_help_popup(frame, frame.area(), keymap),
        Mode::Normal => {}
    }
}

pub(super) fn ui_layout(area: Rect) -> UiLayout {
    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(2)])
        .split(area);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(44), Constraint::Min(40)])
        .split(main[0]);

    UiLayout {
        files: body[0],
        diff: body[1],
        footer: main[1],
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
    let mark = if file.viewed { "✓" } else { "•" };
    let style = if file.viewed {
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
        Span::styled(mark, Style::default().fg(Color::Green)),
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
                    "Use t for trunk()..@, p for @-..@, b for chooser, or pass --base/--rev.",
                ),
                Line::from(generated_hint),
            ])
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title("diff"))
            .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }

    let rows = session.diff_rows_for_selected_file();
    // Lazy rendering: only construct styled lines for the visible window.
    // Rows before the scroll offset are counted (a row emits one line plus
    // one line per attached comment) but never built, so huge files cost
    // O(viewport) per frame instead of O(file).
    let scroll = session.diff_scroll as usize;
    let viewport_lines = inner_bordered(area).height as usize;
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
        let selected = session.focus == Focus::Diff && session.diff_cursor == index;
        let in_range = session.diff_row_in_active_range(index);
        let style = diff_row_style(row.kind, selected, in_range);
        let lineno = row
            .new_lineno
            .or(row.old_lineno)
            .map(|n| format!("{n:>4}"))
            .unwrap_or_else(|| "    ".to_owned());
        let comment_count = comments.len();
        let flagged = session.diff_row_flagged(row);
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
        } else {
            (" ".to_owned(), Style::default().fg(Color::Yellow))
        };

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
                row.text.clone(),
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            )),
            DiffRowKind::Raw => Line::from(row.text.clone()),
            DiffRowKind::Placeholder => Line::from(Span::styled(
                format!("  ⊘ {}", row.text),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )),
            DiffRowKind::ContextFold => Line::from(Span::styled(
                format!("      {}", row.text),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )),
            DiffRowKind::DiffLine(_) => {
                let mut spans = vec![
                    Span::styled(comment_mark, mark_style),
                    Span::styled(lineno, Style::default().fg(Color::DarkGray)),
                    Span::raw(" "),
                    Span::styled(row.prefix, style),
                    Span::raw(" "),
                ];
                spans.extend(diff_text_spans(
                    row,
                    style,
                    selected,
                    in_range,
                    &session.syntax.theme,
                ));
                Line::from(spans)
            }
        };
        if in_window(line_index) {
            lines.push(line);
        }
        line_index += 1;
        for comment in comments {
            if in_window(line_index) {
                lines.push(comment_summary_line(comment));
            }
            line_index += 1;
        }
    }

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("diff"))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
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
    Line::from(vec![
        Span::styled("      ↳ ", Style::default().fg(Color::Yellow)),
        Span::styled(format!("{short_id} "), Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("[{}] ", comment.state.label()),
            comment_state_style(comment.state),
        ),
        Span::styled(summary.to_owned(), Style::default().fg(Color::Yellow)),
    ])
}

fn comment_state_style(state: crate::state::CommentState) -> Style {
    match state {
        crate::state::CommentState::Draft => Style::default().fg(Color::DarkGray),
        crate::state::CommentState::Todo => Style::default().fg(Color::Red),
        crate::state::CommentState::Resolved => Style::default().fg(Color::Green),
    }
}

fn diff_text_spans<'a>(
    row: &'a DiffRow,
    fallback_style: Style,
    selected: bool,
    in_range: bool,
    theme: &SyntaxThemeConfig,
) -> Vec<Span<'a>> {
    if row.syntax.is_empty() {
        return vec![Span::styled(row.text.clone(), fallback_style)];
    }
    row.syntax
        .iter()
        .map(|span| {
            Span::styled(
                span.text.clone(),
                syntax_span_style(span, selected, in_range, theme),
            )
        })
        .collect()
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
    spec.split_whitespace()
        .fold(Style::default(), |style, token| match token {
            "black" => style.fg(Color::Black),
            "blue" => style.fg(Color::Blue),
            "cyan" => style.fg(Color::Cyan),
            "dark-gray" | "dark-grey" => style.fg(Color::DarkGray),
            "gray" | "grey" => style.fg(Color::Gray),
            "green" => style.fg(Color::Green),
            "magenta" => style.fg(Color::Magenta),
            "red" => style.fg(Color::Red),
            "white" => style.fg(Color::White),
            "yellow" => style.fg(Color::Yellow),
            "bold" => style.add_modifier(Modifier::BOLD),
            "dim" => style.add_modifier(Modifier::DIM),
            "italic" => style.add_modifier(Modifier::ITALIC),
            "underlined" | "underline" => style.add_modifier(Modifier::UNDERLINED),
            _ => style,
        })
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
) {
    let mode_text = match mode {
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
            "{kind} comment · {newline} newline · {submit} save · {cancel} cancel",
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
                "choose base/tip · type filter · tab side · {down}/{up} move · enter load · esc cancel",
                down = keymap.hint(Action::TargetPickerMoveDown),
                up = keymap.hint(Action::TargetPickerMoveUp),
            )
        }
        Mode::RevsetInput(_) => {
            "revset target · type revset · tab/↑/↓ switch field · enter load · esc cancel"
                .to_owned()
        }
        Mode::OperationPicker(_) => {
            format!(
                "prior operation · {down}/{up} or j/k move · enter compare · esc cancel",
                down = keymap.hint(Action::TargetPickerMoveDown),
                up = keymap.hint(Action::TargetPickerMoveUp),
            )
        }
        Mode::JjHelpers(state) => {
            if state.confirming {
                "confirm jj command · enter run · esc back".to_owned()
            } else {
                "jj helpers · j/k move · enter select · esc close".to_owned()
            }
        }
        Mode::FlagList(_) => "agent flags · j/k move · enter jump · esc close".to_owned(),
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
            "comments · j/k move · enter jump · s cycle state · x delete · esc close".to_owned()
        }
        Mode::Help => "help · any key to close".to_owned(),
    };
    let mut lines = vec![Line::from(session.summary_line()), Line::from(mode_text)];
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

    let left = vec![
        section("general"),
        entry(
            &[Action::ToggleFocus],
            "switch focus between files and diff",
        ),
        entry(&[Action::Help], "this help"),
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
        entry(&[Action::SymbolOutline], "changed symbol outline"),
        entry(&[Action::ToggleContextFold], "fold/unfold context lines"),
        entry(&[Action::ToggleLargeDiff], "expand/collapse huge diff"),
    ];
    let right = vec![
        section("comments"),
        entry(&[Action::Comment], "comment at cursor"),
        entry(&[Action::RangeComment], "start/finish range comment"),
        entry(&[Action::EditComment], "edit comment"),
        entry(&[Action::DeleteComment], "delete comment"),
        entry(&[Action::CommentList], "comment list"),
        entry(
            &[Action::NextComment, Action::PreviousComment],
            "next/previous comment",
        ),
        section("targets & jj"),
        entry(&[Action::CompareTrunk], "compare trunk()..@"),
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
        entry(&[Action::ToggleAgentOrder], "toggle agent-suggested order"),
        entry(&[Action::FlagList], "agent-flagged sections"),
        entry(&[Action::ChunkList], "agent review chunks"),
        entry(&[Action::DraftList], "agent draft comments"),
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

fn draw_comment_popup(frame: &mut ratatui::Frame<'_>, area: Rect, editor: &CommentEditor) {
    let popup = centered_rect(70, 40, area);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(editor.text.clone())
            .block(Block::default().borders(Borders::ALL).title("comment"))
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

fn draw_flag_list_popup(frame: &mut ratatui::Frame<'_>, area: Rect, list: &FlagListState) {
    let popup = centered_rect(80, 60, area);
    frame.render_widget(Clear, popup);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 3usize;
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
                    let location = match &row.part {
                        Some(part) => match (part.start_line, part.end_line) {
                            (Some(start), Some(end)) => format!("{}:{start}-{end}", part.path),
                            (Some(start), None) => format!("{}:{start}", part.path),
                            _ => part.path.clone(),
                        },
                        None => "(no location)".to_owned(),
                    };
                    let rationale = row
                        .rationale
                        .as_deref()
                        .map(|rationale| format!(" — {rationale}"))
                        .unwrap_or_default();
                    Line::from(vec![
                        Span::styled(format!("{marker} {}{position} ", row.title), style),
                        Span::styled(location, Style::default().fg(Color::Cyan)),
                        Span::styled(rationale, Style::default().fg(Color::DarkGray)),
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
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("review chunks"),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
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
        "Compare against a prior operation: unchanged files are marked viewed",
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
        "↑/↓ or j/k move · enter compare · esc cancel",
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
                    Line::from(vec![
                        Span::styled(format!("{marker} "), style),
                        Span::styled(
                            format!("[{:^8}] ", comment.state.label()),
                            comment_state_style(comment.state),
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
        "↑/↓ or j/k move · enter jump · s cycle state · x delete · esc close",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("comments"))
            .wrap(Wrap { trim: false }),
        popup,
    );
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
        &row.description
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
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        terminal
            .draw(|frame| draw(frame, session, mode, &keymap, None))
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
            .draw(|frame| draw(frame, session, mode, &keymap, None))
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
        let session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1 +1 @@
-old
+new
"#,
        );
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
        let todo_id = session.comments[1].id.clone();
        session.cycle_comment_state(&todo_id);
        let mode = Mode::CommentList(CommentListState { selected: 1 });

        insta::assert_snapshot!(render_tui_text(&session, &mode, 100, 24));
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
                rationale: Some("start here".to_owned()),
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
