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
    editor::CommentEditor,
    keymap::{Action, KeyMap},
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
        Mode::CommentInput { editor, .. } => draw_comment_popup(frame, frame.area(), editor),
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
    ListItem::new(Line::from(vec![
        Span::raw("  ".repeat(row.depth.min(8))),
        Span::styled(mark, Style::default().fg(Color::Green)),
        Span::raw(" "),
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
    let mut lines = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let selected = session.focus == Focus::Diff && session.diff_cursor == index;
        let in_range = session.diff_row_in_active_range(index);
        let style = diff_row_style(row.kind, selected, in_range);
        let lineno = row
            .new_lineno
            .or(row.old_lineno)
            .map(|n| format!("{n:>4}"))
            .unwrap_or_else(|| "    ".to_owned());
        let comment_count = row
            .anchor
            .as_ref()
            .map(|anchor| session.comments_for_diff_row_anchor(anchor))
            .unwrap_or(0);
        let comment_mark = if comment_count > 0 {
            match comment_count {
                1..=9 => comment_count.to_string(),
                _ => "+".to_owned(),
            }
        } else if in_range {
            "|".to_owned()
        } else {
            " ".to_owned()
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
            DiffRowKind::DiffLine(_) => {
                let mut spans = vec![
                    Span::styled(comment_mark, Style::default().fg(Color::Yellow)),
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
        lines.push(line);
        if let Some(anchor) = row.anchor.as_ref() {
            for comment in session.comments_for_diff_row_anchor_details(anchor) {
                lines.push(comment_summary_line(comment));
            }
        }
    }

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("diff"))
        .scroll((session.diff_scroll, 0))
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
        Span::styled(summary.to_owned(), Style::default().fg(Color::Yellow)),
    ])
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
        Mode::Normal if session.focus == Focus::Files => format!(
            "{} · focus files{} · {down}/{up} tree · {fold} fold · {generated} {noisy_label} · {trunk}/{parent}/{choose} target · {next_unviewed}/{previous_unviewed} unviewed · {next_comment}/{previous_comment} comments · {focus} diff · {mark} viewed · {toggle} toggle · {comment}/{edit}/{delete} comment · {quit} quit",
            session.target,
            if session.hide_generated {
                " (noisy hidden)"
            } else {
                ""
            },
            down = keymap.hint(Action::MoveDown),
            up = keymap.hint(Action::MoveUp),
            fold = keymap.hint(Action::ToggleFold),
            generated = keymap.hint(Action::ToggleGenerated),
            noisy_label = noisy_toggle_label(session),
            trunk = keymap.hint(Action::CompareTrunk),
            parent = keymap.hint(Action::CompareParent),
            choose = keymap.hint(Action::TargetChooser),
            next_unviewed = keymap.hint(Action::NextUnviewed),
            previous_unviewed = keymap.hint(Action::PreviousUnviewed),
            next_comment = keymap.hint(Action::NextComment),
            previous_comment = keymap.hint(Action::PreviousComment),
            focus = keymap.hint(Action::ToggleFocus),
            mark = keymap.hint(Action::MarkViewed),
            toggle = keymap.hint(Action::ToggleViewed),
            comment = keymap.hint(Action::Comment),
            edit = keymap.hint(Action::EditComment),
            delete = keymap.hint(Action::DeleteComment),
            quit = keymap.hint(Action::Quit),
        ),
        Mode::Normal => format!(
            "{} · focus diff{}{} · {down}/{up} line · {range} range · {cancel_range} cancel · {generated} {noisy_label} · {trunk}/{parent}/{choose} target · {next_unviewed}/{previous_unviewed} unviewed · {next_comment}/{previous_comment} comments · {focus} files · {comment}/{edit}/{delete} comment · {scroll_down}/{scroll_up} scroll · {quit} quit",
            session.target,
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
            down = keymap.hint(Action::MoveDown),
            up = keymap.hint(Action::MoveUp),
            range = keymap.hint(Action::RangeComment),
            cancel_range = keymap.hint(Action::CancelRangeComment),
            generated = keymap.hint(Action::ToggleGenerated),
            noisy_label = noisy_toggle_label(session),
            trunk = keymap.hint(Action::CompareTrunk),
            parent = keymap.hint(Action::CompareParent),
            choose = keymap.hint(Action::TargetChooser),
            next_unviewed = keymap.hint(Action::NextUnviewed),
            previous_unviewed = keymap.hint(Action::PreviousUnviewed),
            next_comment = keymap.hint(Action::NextComment),
            previous_comment = keymap.hint(Action::PreviousComment),
            focus = keymap.hint(Action::ToggleFocus),
            comment = keymap.hint(Action::Comment),
            edit = keymap.hint(Action::EditComment),
            delete = keymap.hint(Action::DeleteComment),
            scroll_down = keymap.hint(Action::ScrollDown),
            scroll_up = keymap.hint(Action::ScrollUp),
            quit = keymap.hint(Action::Quit),
        ),
        Mode::CommentInput { target, .. } => format!(
            "{kind} comment · {newline} newline · {submit} save · {cancel} cancel",
            kind = match target {
                CommentInputTarget::New => "new",
                CommentInputTarget::Edit { .. } => "edit",
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

fn noisy_toggle_label(session: &ReviewSession) -> &'static str {
    if session.hide_generated {
        "show noisy"
    } else {
        "hide noisy"
    }
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
            created_at: chrono::Utc::now(),
        });
        session.comments.push(Comment {
            id: "c2".to_owned(),
            path: anchor.path().to_owned(),
            line: anchor.line(),
            end_line: None,
            anchor: Some(anchor),
            body: "second note".to_owned(),
            created_at: chrono::Utc::now(),
        });

        let rendered = render_tui_text(&session, &Mode::Normal, 100, 16);

        assert!(rendered.contains("2   1 - old"));
        assert!(rendered.contains("↳ c1 first note"));
        assert!(rendered.contains("↳ c2 second note"));
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
