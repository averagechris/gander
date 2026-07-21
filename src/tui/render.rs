//! All drawing code: panes, popups, styles, and layout math.

use std::{
    collections::{BTreeSet, HashMap, hash_map::DefaultHasher},
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
    config::{DiffViewModeConfig, UiConfig},
    diff::DiffLineKind,
    file_tree::{FlatTreeRow, FlatTreeRowKind},
    jj::JjChangeSummary,
    state::{Channel, Comment, ReviewTarget, Salience},
    syntax::{HighlightKind, SyntaxSpan, SyntaxThemeConfig},
};

use super::{
    ActivityListState, CommentInputTarget, Mode, TuiState, UiNotice, UiNoticeLevel,
    action_items::{OpenWorkListState, OpenWorkRow},
    annotation_card::{
        AnnotationCard, AnnotationCardDensity, AnnotationCardLayout, AnnotationSource,
    },
    chooser::TargetChooserState,
    comments::CommentListState,
    drafts::DraftListState,
    editor::CommentEditor,
    flags::FlagListState,
    glance::GlanceBoardState,
    helpers::{JjHelperOption, JjHelperState},
    keymap::{Action, KeyMap},
    ops::OperationPickerState,
    outline::SymbolOutlineState,
    revset::{RevsetField, RevsetInputState},
    search::FileSearchState,
    text_layout::VisualTextLayout,
    theme::AppTheme,
    view_options::{ViewOption, ViewOptionsState},
    walkthroughs::WalkthroughListState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UiLayout {
    pub(super) menu: Rect,
    pub(super) files: Rect,
    pub(super) diff: Rect,
    pub(super) footer: Rect,
}

struct FooterContext<'a> {
    mode: &'a Mode,
    keymap: &'a KeyMap,
    identity_chip: Option<&'a str>,
    notice: Option<&'a UiNotice>,
    attention_focus: bool,
}

fn footer_context<'a>(
    mode: &'a Mode,
    keymap: &'a KeyMap,
    tui_state: &'a TuiState,
    notice: Option<&'a UiNotice>,
) -> FooterContext<'a> {
    FooterContext {
        mode,
        keymap,
        identity_chip: tui_state.current_identity_chip.as_deref(),
        notice,
        attention_focus: tui_state.attention_focus.is_some(),
    }
}

pub(super) fn draw(
    frame: &mut ratatui::Frame<'_>,
    session: &ReviewSession,
    mode: &Mode,
    keymap: &KeyMap,
    tui_state: &TuiState,
    notice: Option<&UiNotice>,
) {
    let theme = &tui_state.theme;
    tui_state.diff_viewport.set_annotation_artifact_hint(
        keymap
            .bound_hint(Action::ToggleAnnotationArtifacts)
            .unwrap_or("unbound"),
    );
    let full_area = frame.area();
    frame.buffer_mut().set_style(full_area, theme.base_style());
    let effective_file_pane = tui_state.effective_file_pane(session, full_area.width);
    let layout = tui_state.review_layout(session, full_area);

    if layout.files.width > 0 {
        draw_files(frame, layout.files, session, theme);
    }
    draw_menu_bar(frame, layout.menu, keymap, theme);
    draw_diff(
        frame,
        layout.diff,
        session,
        tui_state,
        effective_file_pane.visible,
    );
    draw_footer(
        frame,
        layout.footer,
        session,
        &footer_context(mode, keymap, tui_state, notice),
        theme,
    );

    match mode {
        Mode::TargetChooser(chooser) => {
            draw_target_chooser_popup(frame, frame.area(), chooser, keymap, theme)
        }
        Mode::RevsetInput(input) => {
            draw_revset_input_popup(frame, frame.area(), input, keymap, theme)
        }
        Mode::OperationPicker(picker) => {
            draw_operation_picker_popup(frame, frame.area(), picker, keymap, theme)
        }
        Mode::JjHelpers(state) => draw_jj_helpers_popup(frame, frame.area(), state, keymap, theme),
        Mode::FlagList(list) => draw_flag_list_popup(frame, frame.area(), list, keymap, theme),
        Mode::OpenWork(list) => {
            draw_open_work_popup(frame, frame.area(), session, list, keymap, theme)
        }
        Mode::Activity(list) => draw_activity_popup(frame, frame.area(), tui_state, list),
        Mode::WalkthroughList(list) => {
            draw_walkthrough_list_popup(frame, frame.area(), session, list, keymap, theme)
        }
        Mode::DraftList(list) => draw_draft_list_popup(frame, frame.area(), list, keymap, theme),
        Mode::FileSearch(search) => {
            draw_file_search_popup(frame, frame.area(), search, keymap, theme)
        }
        Mode::SymbolOutline(outline) => {
            draw_symbol_outline_popup(frame, frame.area(), outline, keymap, theme)
        }
        Mode::CommentList(list) => {
            draw_comment_list_popup(frame, frame.area(), session, list, keymap, theme)
        }
        Mode::ViewOptions(state) => draw_view_options_popup(
            frame,
            frame.area(),
            session,
            state,
            keymap,
            theme,
            effective_file_pane.visible,
        ),
        Mode::AttentionGlance(board) => {
            draw_attention_glance_popup(frame, frame.area(), board, keymap, theme)
        }
        Mode::CommentInput { editor, target } => {
            draw_comment_popup(frame, frame.area(), session, editor, target, keymap, theme)
        }
        Mode::Help => draw_help_popup(frame, frame.area(), keymap, tui_state.help_scroll, theme),
        Mode::Normal => {}
    }
}

pub(super) fn ui_layout(
    area: Rect,
    files_visible: bool,
    config: &UiConfig,
    split_percent: u16,
) -> UiLayout {
    let menu_height = if config.menu_bar && area.height >= 18 && area.width >= 80 {
        1
    } else {
        0
    };
    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(menu_height),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(area);
    let files_width = if files_visible {
        let requested = (area.width as u32 * split_percent.clamp(10, 60) as u32 / 100) as u16;
        let max_files = area.width.saturating_sub(40);
        requested.clamp(20, max_files.max(20))
    } else {
        0
    };
    let body = Layout::default()
        .direction(Direction::Horizontal)
        // `files_width` already reserves 40 columns for the diff whenever the
        // terminal is wide enough. If an explicit override forces the pane on
        // below that width, preserve its 20-column minimum and let the diff use
        // the remainder instead of allowing ratatui to squeeze files away.
        .constraints([Constraint::Length(files_width), Constraint::Min(0)])
        .split(main[1]);

    UiLayout {
        menu: main[0],
        files: body[0],
        diff: body[1],
        footer: main[2],
    }
}

fn draw_menu_bar(frame: &mut ratatui::Frame<'_>, area: Rect, keymap: &KeyMap, theme: &AppTheme) {
    if area.height == 0 || area.width < 80 {
        return;
    }
    let items = [
        ("help", Action::Help),
        ("hunk", Action::NextChangedHunk),
        ("file", Action::NextFile),
        ("comment", Action::Comment),
        ("view", Action::ViewOptions),
        ("focus", Action::AttentionFocus),
        ("glance", Action::AttentionGlance),
        ("pane", Action::ToggleFilePane),
        ("quit", Action::Quit),
    ];
    let mut text = String::from(" ");
    let mut rendered_any = false;
    for (label, action) in items {
        let Some(hint) = keymap.bound_hint(action) else {
            continue;
        };
        if rendered_any {
            text.push_str("  ");
        }
        text.push_str(hint);
        text.push(' ');
        text.push_str(label);
        rendered_any = true;
    }
    frame.render_widget(Paragraph::new(text).style(theme.base_style()), area);
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

/// Clear a popup area and re-establish the themed base style so popup
/// text never bypasses AppTheme::foreground (and paints the base
/// background in opaque mode).
fn clear_popup(frame: &mut ratatui::Frame<'_>, area: Rect, theme: &AppTheme) {
    frame.render_widget(Clear, area);
    frame.buffer_mut().set_style(area, theme.base_style());
}

fn draw_attention_glance_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    board: &GlanceBoardState,
    keymap: &KeyMap,
    theme: &AppTheme,
) {
    let popup = centered_rect(if area.width < 72 { 96 } else { 88 }, 72, area);
    clear_popup(frame, popup, theme);
    let footer = format!(
        "{} jump  {} peek  {} acknowledge  {} acknowledge all  {} close",
        keymap.hint(Action::PopupSelect),
        keymap.hint(Action::GlancePeek),
        keymap.hint(Action::GlanceAcknowledge),
        keymap.hint(Action::GlanceAcknowledgeAll),
        keymap.hint(Action::PopupClose),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(
            " attention glance · {} skim folds ",
            board.rows.len()
        ))
        .title_bottom(Line::from(truncate_tail(
            &footer,
            popup.width.saturating_sub(4) as usize,
        )))
        .style(theme.base_style());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if board.rows.is_empty() {
        frame.render_widget(
            Paragraph::new("No skim folds on the effective attention map")
                .style(Style::default().fg(theme.muted)),
            inner,
        );
        return;
    }
    let width = inner.width.saturating_sub(3) as usize;
    let items = board
        .rows
        .iter()
        .map(|row| {
            let state = if row.stale {
                "stale"
            } else if row.acknowledged {
                "✓ acknowledged"
            } else {
                "current"
            };
            let paths = if row.paths.is_empty() {
                "(missing target)".to_owned()
            } else {
                row.paths.join(", ")
            };
            let first = format!(
                "{} · {} file{} · +{} −{} · {}",
                state,
                row.file_count,
                if row.file_count == 1 { "" } else { "s" },
                row.additions,
                row.deletions,
                row.rationale
            );
            ListItem::new(vec![
                Line::from(Span::styled(
                    truncate_tail(&first, width),
                    Style::default().fg(if row.stale {
                        theme.muted
                    } else if row.acknowledged {
                        theme.positive
                    } else {
                        theme.accent
                    }),
                )),
                Line::from(Span::styled(
                    truncate_tail(&paths, width),
                    Style::default().fg(theme.detail),
                )),
            ])
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(board.selected));
    frame.render_stateful_widget(
        List::new(items).highlight_symbol("› ").highlight_style(
            Style::default()
                .bg(theme.selection_bg)
                .add_modifier(Modifier::BOLD),
        ),
        inner,
        &mut state,
    );
}

fn draw_files(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    theme: &AppTheme,
) {
    let tree = session.file_tree();
    if tree.rows.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from("No changed files"),
                Line::from(""),
                Line::from("Try t for trunk, p for parent, b for target chooser, or adjust --base/--rev/--ignore."),
            ])
            .style(Style::default().fg(theme.muted))
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
            FlatTreeRowKind::Directory { collapsed } => {
                render_directory_row(row, *collapsed, theme)
            }
            FlatTreeRowKind::File { file_index } => {
                render_file_row(row, session, *file_index, theme)
            }
        })
        .collect();

    let mut state = ListState::default().with_selected(session.selected_tree_row(&tree));
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("files"))
        .highlight_style(
            Style::default()
                .bg(theme.selection_bg)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_directory_row(row: &FlatTreeRow, collapsed: bool, theme: &AppTheme) -> ListItem<'static> {
    let indent = "  ".repeat(row.depth.min(8));
    let glyph = if collapsed { " ▸ " } else { " ▾ " };
    ListItem::new(Line::from(vec![
        Span::raw(indent),
        Span::styled(row.stats.mark(), Style::default().fg(theme.positive)),
        Span::raw(glyph),
        Span::styled(
            row.label.clone(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {}/{}", row.stats.viewed, row.stats.total),
            Style::default().fg(theme.muted),
        ),
    ]))
}

fn render_file_row(
    row: &FlatTreeRow,
    session: &ReviewSession,
    file_index: usize,
    theme: &AppTheme,
) -> ListItem<'static> {
    let file = &session.files[file_index];
    let reviewed = file.viewed || file.caught_up;
    let (mark, mark_style) = if file.changed_since_look && reviewed {
        ("~", Style::default().fg(theme.accent))
    } else if file.changed_since_look {
        (
            "±",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
    } else if file.viewed_stale {
        ("~", Style::default().fg(theme.accent))
    } else if file.viewed {
        ("✓", Style::default().fg(theme.positive))
    } else if file.caught_up {
        ("◌", Style::default().fg(theme.muted))
    } else {
        ("•", Style::default().fg(theme.positive))
    };
    let style = if reviewed {
        Style::default().fg(theme.muted)
    } else {
        Style::default().fg(theme.foreground)
    };
    let flag_span = if session.file_has_flags(&file.path) {
        Span::styled(
            "!",
            Style::default()
                .fg(theme.negative)
                .add_modifier(Modifier::BOLD),
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
            Style::default().fg(theme.detail),
        ),
        Span::raw(" "),
        Span::styled(
            if file.generated { "gen " } else { "    " },
            Style::default().fg(theme.secondary),
        ),
        Span::styled(row.label.clone(), style),
    ]))
}

fn draw_diff(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    tui_state: &TuiState,
    file_pane_visible: bool,
) {
    let theme = &tui_state.theme;
    if if session.stream_mode {
        session.review_stream().rows.is_empty()
    } else {
        session.selected_visible_file().is_none()
    } {
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
                        .fg(theme.accent)
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
            .style(Style::default().fg(theme.muted))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(diff_pane_title(session, file_pane_visible)),
            )
            .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }

    let inner = inner_bordered(area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let split_requested = session.diff_cues.view == DiffViewModeConfig::SideBySide;
    let split_active = diff_split_is_active(session, inner);
    let measured = tui_state
        .diff_viewport
        .measure(session, inner, split_active);
    let window = tui_state
        .diff_viewport
        .window(session, &measured, inner.height as usize);
    let lines = measured.materialize(
        session,
        window.start,
        inner.height as usize,
        window.horizontal,
        &tui_state.theme,
    );

    let mut title = diff_pane_title(session, file_pane_visible);
    if split_requested && !split_active {
        // Two unreadable half-panes help nobody: fall back to unified on
        // narrow terminals and say so in the title.
        title.push_str(" · unified (narrow)");
    }
    let horizontal = window.horizontal;
    if !session.diff_cues.soft_wrap && horizontal > 0 {
        title.push_str(&format!(" · x:{horizontal}"));
    }
    let paragraph =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(paragraph, area);
}

/// Minimum inner width (columns) for the side-by-side layout.
pub(super) const MIN_SPLIT_WIDTH: u16 = 100;

/// The single split/fallback decision shared by drawing and viewport geometry.
pub(super) fn diff_split_is_active(session: &ReviewSession, inner: Rect) -> bool {
    session.diff_cues.view == DiffViewModeConfig::SideBySide && inner.width >= MIN_SPLIT_WIDTH
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiffVisualHit {
    Full(usize),
    Split {
        left: Option<usize>,
        right: Option<usize>,
        divider: usize,
    },
    Annotation {
        owner: usize,
        source: AnnotationSource,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DiffPointHit {
    Code(usize),
    Annotation {
        owner: usize,
        source: AnnotationSource,
    },
}

impl DiffVisualHit {
    fn contains(&self, row: usize) -> bool {
        match self {
            Self::Full(owner) => *owner == row,
            Self::Split { left, right, .. } => *left == Some(row) || *right == Some(row),
            Self::Annotation { owner, .. } => *owner == row,
        }
    }

    fn row_at(&self, column: usize) -> Option<usize> {
        match self {
            Self::Full(row) => Some(*row),
            Self::Split {
                left,
                right,
                divider,
            } => {
                if column < *divider {
                    *left
                } else {
                    *right
                }
            }
            Self::Annotation { owner, .. } => Some(*owner),
        }
    }

    fn point_at(&self, column: usize) -> Option<DiffPointHit> {
        match self {
            Self::Full(row) => Some(DiffPointHit::Code(*row)),
            Self::Split { .. } => self.row_at(column).map(DiffPointHit::Code),
            Self::Annotation { owner, source } => Some(DiffPointHit::Annotation {
                owner: *owner,
                source: source.clone(),
            }),
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
    Annotation {
        owner: usize,
        card: Rc<AnnotationCard>,
        layout: Rc<AnnotationCardLayout>,
        line: usize,
        width: usize,
    },
}

#[derive(Debug, Clone)]
pub(super) struct DiffVisualLine {
    source: DiffVisualSource,
    hit: DiffVisualHit,
    block_anchor: usize,
    is_comment: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct MeasuredDiffLayout {
    pub(super) lines: Vec<DiffVisualLine>,
    pub(super) line_number_width: usize,
    width: usize,
    pub(super) horizontal_limit: usize,
}

/// Cheap selected-file signature used before a layout cache hit. It owns no
/// reply/artifact payloads; visible text is hashed in place from durable state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct AnnotationLayoutInput {
    pub(super) changed_hunks: Vec<usize>,
    cards: Vec<AnnotationCardSignature>,
    artifact_hint: String,
}

impl AnnotationLayoutInput {
    pub(super) fn contains_source(&self, source: &AnnotationSource) -> bool {
        self.cards.iter().any(|card| &card.source == source)
    }

    pub(super) fn owner_for_source(&self, source: &AnnotationSource) -> Option<usize> {
        self.cards
            .iter()
            .find(|card| &card.source == source)
            .map(|card| card.owner)
    }

    pub(super) fn walkthrough_source_at_owner(&self, owner: usize) -> Option<AnnotationSource> {
        self.cards
            .iter()
            .find(|card| {
                card.owner == owner && matches!(card.source, AnnotationSource::Walkthrough { .. })
            })
            .map(|card| card.source.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AnnotationCardSignature {
    owner: usize,
    source: AnnotationSource,
    geometry_hash: u64,
    artifacts_expanded: bool,
}

#[derive(Debug, Clone)]
struct AnnotationCardInput {
    owner: usize,
    card: Rc<AnnotationCard>,
    artifacts_expanded: bool,
}

/// Geometry input for selected-file comments and spotlight narration.
#[derive(Debug, Clone, Default)]
pub(super) struct SelectedFileAnnotations {
    pub(super) input: AnnotationLayoutInput,
    cards: Vec<AnnotationCardInput>,
}

impl MeasuredDiffLayout {
    pub(super) fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub(super) fn cursor_bounds(&self, cursor: usize) -> Option<(usize, usize)> {
        let first = self
            .lines
            .iter()
            .position(|line| !line.is_comment && line.hit.contains(cursor))?;
        let last = self
            .lines
            .iter()
            .rposition(|line| !line.is_comment && line.hit.contains(cursor))?;
        Some((first, last))
    }

    pub(super) fn cursor_visible(&self, cursor: usize, start: usize, height: usize) -> bool {
        self.lines[start.min(self.lines.len())..start.saturating_add(height).min(self.lines.len())]
            .iter()
            .any(|line| !line.is_comment && line.hit.contains(cursor))
    }

    pub(super) fn row_at(&self, line: usize, column: usize) -> Option<usize> {
        self.lines.get(line)?.hit.row_at(column)
    }

    pub(super) fn hit_at(&self, line: usize, column: usize) -> Option<DiffPointHit> {
        self.lines.get(line)?.hit.point_at(column)
    }

    #[cfg(test)]
    pub(super) fn annotation_line(&self, source: &AnnotationSource) -> Option<usize> {
        self.lines.iter().position(|line| {
            matches!(
                &line.hit,
                DiffVisualHit::Annotation { source: candidate, .. } if candidate == source
            )
        })
    }

    pub(super) fn viewport_start(
        &self,
        logical: usize,
        continuation: usize,
        viewport_height: usize,
    ) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
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
            .saturating_add(continuation)
            .min(block_end.saturating_sub(1));
        let maximum_top = self.lines.len().saturating_sub(viewport_height.max(1));
        requested.min(maximum_top)
    }

    pub(super) fn viewport_from_line(&self, index: usize) -> (u16, usize) {
        let index = index.min(self.lines.len().saturating_sub(1));
        let Some(line) = self.lines.get(index) else {
            return (0, 0);
        };
        let first = self.lines[..=index]
            .iter()
            .rposition(|candidate| candidate.block_anchor != line.block_anchor)
            .map_or(0, |prior| prior + 1);
        (
            line.block_anchor.min(u16::MAX as usize) as u16,
            index.saturating_sub(first),
        )
    }
}

/// Build the complete terminal-row projection. Drawing, visual scrolling,
/// and mouse hit testing all consume this exact measured geometry.
#[cfg(test)]
pub(super) fn cached_diff_layout(
    session: &ReviewSession,
    rows: Rc<Vec<DiffRow>>,
    inner: Rect,
    split_active: bool,
    tui_state: &TuiState,
) -> Rc<MeasuredDiffLayout> {
    tui_state
        .diff_viewport
        .measure_rows(session, rows, inner, split_active)
        .test_layout_rc()
}

#[cfg(test)]
pub(super) fn selected_file_annotations(
    session: &ReviewSession,
    rows: &[DiffRow],
) -> SelectedFileAnnotations {
    selected_file_annotations_with_expansion(session, rows, &BTreeSet::new(), "E")
}

#[cfg(test)]
pub(super) fn selected_file_annotations_with_expansion(
    session: &ReviewSession,
    _rows: &[DiffRow],
    expanded: &BTreeSet<String>,
    artifact_hint: &str,
) -> SelectedFileAnnotations {
    let input = selected_file_annotation_input(session, expanded, artifact_hint);
    selected_file_annotations_for_input(session, input, expanded)
}

pub(super) fn selected_file_annotation_input(
    session: &ReviewSession,
    expanded: &BTreeSet<String>,
    artifact_hint: &str,
) -> AnnotationLayoutInput {
    // Hunk indices are file-local and therefore ambiguous in a cross-file
    // stream. Freshness remains visible in the file tree; stream cards and
    // anchors use their stable file-qualified owners.
    let changed_hunks = if session.stream_mode {
        Vec::new()
    } else {
        session
            .selected_visible_file()
            .map(|file| file.changed_hunks.iter().copied().collect())
            .unwrap_or_default()
    };
    let mut signatures = Vec::new();
    for comment in &session.comments {
        let owner = if session.stream_mode {
            session.stream_comment_card_owner(comment)
        } else {
            session.selected_comment_card_owner(comment)
        };
        let Some(owner) = owner else {
            continue;
        };
        let source = AnnotationSource::Comment {
            id: comment.id.clone(),
        };
        signatures.push(AnnotationCardSignature {
            owner,
            geometry_hash: comment_geometry_hash(comment),
            artifacts_expanded: false,
            source,
        });
    }
    for_each_effective_walkthrough_card(session, |step, target, part, owner, rationale| {
        let source = AnnotationSource::Walkthrough {
            step_id: step.id.clone(),
            part,
        };
        let artifacts_expanded = expanded.contains(&source.stable_id());
        signatures.push(AnnotationCardSignature {
            owner,
            geometry_hash: walkthrough_geometry_hash(
                step,
                target,
                rationale.as_deref(),
                artifacts_expanded,
            ),
            artifacts_expanded,
            source,
        });
    });
    AnnotationLayoutInput {
        changed_hunks,
        cards: signatures,
        artifact_hint: artifact_hint.to_owned(),
    }
}

pub(super) fn selected_file_annotations_for_input(
    session: &ReviewSession,
    input: AnnotationLayoutInput,
    expanded: &BTreeSet<String>,
) -> SelectedFileAnnotations {
    let mut cards = Vec::with_capacity(input.cards.len());
    for comment in &session.comments {
        let owner = if session.stream_mode {
            session.stream_comment_card_owner(comment)
        } else {
            session.selected_comment_card_owner(comment)
        };
        let Some(owner) = owner else {
            continue;
        };
        cards.push(AnnotationCardInput {
            owner,
            card: Rc::new(AnnotationCard::from_comment(comment)),
            artifacts_expanded: false,
        });
    }
    for_each_effective_walkthrough_card(session, |step, target, part, owner, rationale| {
        let card = Rc::new(AnnotationCard::from_walkthrough_step(
            step,
            target,
            part,
            rationale,
            expanded.contains(
                &AnnotationSource::Walkthrough {
                    step_id: step.id.clone(),
                    part,
                }
                .stable_id(),
            ),
        ));
        cards.push(AnnotationCardInput {
            owner,
            artifacts_expanded: expanded.contains(&card.source.stable_id()),
            card,
        });
    });
    SelectedFileAnnotations { input, cards }
}

fn for_each_effective_walkthrough_card(
    session: &ReviewSession,
    mut visit: impl FnMut(&crate::state::WalkthroughStep, &ReviewTarget, usize, usize, Option<String>),
) {
    let Some(durable) = crate::review::active_session_for_loaded_review(
        &session.sessions,
        &session.repo,
        &session.target.base,
        &session.target.rev,
    ) else {
        return;
    };
    let files = session
        .files
        .iter()
        .map(|file| &file.diff)
        .collect::<Vec<_>>();
    for step in durable
        .walkthroughs
        .iter()
        .flat_map(|walkthrough| walkthrough.steps.iter())
    {
        for (part, target) in std::iter::once(&step.target)
            .chain(step.extra_targets.iter())
            .enumerate()
        {
            if !crate::attention::target_is_current(target, &files) {
                continue;
            }
            let effective =
                crate::attention::resolve_effective_attention_refs(durable, target, &files);
            if effective.salience != Salience::Spotlight {
                continue;
            }
            let owner = if session.stream_mode {
                session.stream_walkthrough_card_owner(target)
            } else {
                session.selected_walkthrough_card_owner(target)
            };
            let Some(owner) = owner else {
                continue;
            };
            visit(step, target, part, owner, effective.rationale);
        }
    }
}

fn comment_geometry_hash(comment: &Comment) -> u64 {
    let mut hash = DefaultHasher::new();
    comment.id.hash(&mut hash);
    comment.body.hash(&mut hash);
    comment.author.name.hash(&mut hash);
    (comment.author.kind as u8).hash(&mut hash);
    comment.channel.audience_label().hash(&mut hash);
    comment.state.label().hash(&mut hash);
    comment.kind.map(|kind| kind as u8).hash(&mut hash);
    comment.action.map(|action| action as u8).hash(&mut hash);
    comment.path.hash(&mut hash);
    comment.line.hash(&mut hash);
    comment.end_line.hash(&mut hash);
    for reply in &comment.replies {
        reply.author.name.hash(&mut hash);
        (reply.author.kind as u8).hash(&mut hash);
        reply.body.hash(&mut hash);
    }
    hash.finish()
}

fn walkthrough_geometry_hash(
    step: &crate::state::WalkthroughStep,
    target: &ReviewTarget,
    rationale: Option<&str>,
    artifacts_expanded: bool,
) -> u64 {
    let mut hash = DefaultHasher::new();
    step.id.hash(&mut hash);
    step.author
        .as_ref()
        .map(|author| &author.name)
        .hash(&mut hash);
    step.author
        .as_ref()
        .map(|author| author.kind as u8)
        .hash(&mut hash);
    step.title.hash(&mut hash);
    step.body.hash(&mut hash);
    step.why.hash(&mut hash);
    rationale.hash(&mut hash);
    target.file.hash(&mut hash);
    target.line.hash(&mut hash);
    target.end_line.hash(&mut hash);
    for artifact in &step.artifacts {
        artifact.title.hash(&mut hash);
        (artifact.kind as u8).hash(&mut hash);
        if artifacts_expanded {
            artifact.body.hash(&mut hash);
        }
    }
    hash.finish()
}

pub(super) fn annotation_artifact_card_at_owner(
    session: &ReviewSession,
    owner: usize,
    preferred: Option<&AnnotationSource>,
) -> Option<AnnotationSource> {
    let mut found = Vec::new();
    for_each_effective_walkthrough_card(session, |step, _, part, card_owner, _| {
        if card_owner == owner && !step.artifacts.is_empty() {
            found.push(AnnotationSource::Walkthrough {
                step_id: step.id.clone(),
                part,
            });
        }
    });
    match preferred {
        Some(preferred) => found.into_iter().find(|candidate| candidate == preferred),
        None => found.into_iter().next(),
    }
}

#[cfg(test)]
fn measured_diff_layout(
    session: &ReviewSession,
    rows: &[DiffRow],
    inner: Rect,
    split_active: bool,
) -> MeasuredDiffLayout {
    let annotations = selected_file_annotations(session, rows);
    measured_diff_layout_with_annotations(session, rows, inner, split_active, &annotations)
}

pub(super) fn measured_diff_layout_with_annotations(
    session: &ReviewSession,
    rows: &[DiffRow],
    inner: Rect,
    split_active: bool,
    annotations: &SelectedFileAnnotations,
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
        measured_split_layout(session, rows, width, line_number_width, annotations)
    } else {
        measured_unified_layout(session, rows, width, line_number_width, annotations)
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
    annotations: &SelectedFileAnnotations,
) -> MeasuredDiffLayout {
    let mut lines = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        for source in
            measured_unified_row(session, row, index, width, line_number_width, annotations)
        {
            lines.push(DiffVisualLine {
                source,
                hit: DiffVisualHit::Full(index),
                block_anchor: index,
                is_comment: false,
            });
        }
        append_comment_lines(&mut lines, annotations, index, width);
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
    annotations: &SelectedFileAnnotations,
) -> MeasuredDiffLayout {
    let mut lines = Vec::new();
    let left_width = width.saturating_sub(1) / 2;
    let right_width = width.saturating_sub(1).saturating_sub(left_width);
    for split in split_rows(rows) {
        match split {
            SplitRow::Full(index) => {
                for source in measured_unified_row(
                    session,
                    &rows[index],
                    index,
                    width,
                    line_number_width,
                    annotations,
                ) {
                    lines.push(DiffVisualLine {
                        source,
                        hit: DiffVisualHit::Full(index),
                        block_anchor: index,
                        is_comment: false,
                    });
                }
                append_comment_lines(&mut lines, annotations, index, width);
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
                    append_comment_lines(&mut lines, annotations, owner, width);
                }
            }
        }
    }
    MeasuredDiffLayout {
        lines,
        ..Default::default()
    }
}

fn row_comment_count(session: &ReviewSession, row: &DiffRow) -> usize {
    let Some(anchor) = row.anchor.as_ref() else {
        return 0;
    };
    session.comments_for_diff_row_anchor_details(anchor).len()
}

fn render_cursor(session: &ReviewSession) -> usize {
    if session.stream_mode {
        session.stream_cursor
    } else {
        session.diff_cursor
    }
}

fn render_row_in_range(session: &ReviewSession, row: usize) -> bool {
    if session.stream_mode {
        session.stream_row_in_active_range(row)
    } else {
        session.diff_row_in_active_range(row)
    }
}

/// Gutter mark color for a commented row. When comments from more than one
/// channel share the row's anchor, the most actionable channel deliberately
/// wins: delegation > collaboration > onboarding > note. Delegation marks
/// agent work to pick up, collaboration marks team-facing threads, onboarding
/// is explanatory narration, and note is private — so the mark surfaces the
/// strongest call to action rather than whichever comment happens to sort
/// first.
fn row_comment_channel(session: &ReviewSession, row: &DiffRow) -> Option<Channel> {
    let anchor = row.anchor.as_ref()?;
    session
        .comments_for_diff_row_anchor_details(anchor)
        .iter()
        .map(|comment| comment.channel)
        .max_by_key(|channel| gutter_channel_priority(*channel))
}

fn gutter_channel_priority(channel: Channel) -> u8 {
    match channel {
        Channel::Delegation => 3,
        Channel::Collaboration => 2,
        Channel::Onboarding => 1,
        Channel::Note => 0,
    }
}

fn append_comment_lines(
    lines: &mut Vec<DiffVisualLine>,
    annotations: &SelectedFileAnnotations,
    owner: usize,
    width: usize,
) {
    for input in annotations
        .cards
        .iter()
        .filter(|input| input.owner == owner)
    {
        let layout = Rc::new(input.card.layout(
            width.max(1),
            AnnotationCardDensity::Expanded,
            input.artifacts_expanded,
            &annotations.input.artifact_hint,
        ));
        for line in 0..layout.len() {
            lines.push(DiffVisualLine {
                source: DiffVisualSource::Annotation {
                    owner,
                    card: Rc::clone(&input.card),
                    layout: Rc::clone(&layout),
                    line,
                    width,
                },
                hit: DiffVisualHit::Annotation {
                    owner,
                    source: input.card.source.clone(),
                },
                block_anchor: owner,
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
    annotations: &SelectedFileAnnotations,
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
    let text = plain_row_text(row, &annotations.input);
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

fn plain_row_text(row: &DiffRow, annotations: &AnnotationLayoutInput) -> String {
    match row.kind {
        DiffRowKind::ChapterHeader | DiffRowKind::SkimFold => row.text.clone(),
        DiffRowKind::FileHeader | DiffRowKind::SyntaxSummary | DiffRowKind::Raw => row.text.clone(),
        DiffRowKind::HunkHeader
            if row
                .hunk_index
                .is_some_and(|hunk| annotations.changed_hunks.binary_search(&hunk).is_ok()) =>
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

#[allow(clippy::too_many_arguments)]
pub(super) fn materialize_diff_window(
    session: &ReviewSession,
    rows: &[DiffRow],
    layout: &MeasuredDiffLayout,
    start: usize,
    height: usize,
    horizontal: usize,
    selected_annotation: Option<&AnnotationSource>,
    theme: &AppTheme,
) -> Vec<Line<'static>> {
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
                selected_annotation,
                &visual.source,
                &mut prepared,
                theme,
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
        None,
        source,
        &mut HashMap::new(),
        &AppTheme::default(),
    )
}

#[allow(clippy::too_many_arguments)]
fn materialize_diff_source_cached(
    session: &ReviewSession,
    rows: &[DiffRow],
    line_number_width: usize,
    horizontal: usize,
    selected_annotation: Option<&AnnotationSource>,
    source: &DiffVisualSource,
    prepared: &mut HashMap<(usize, Option<usize>), PreparedDiffCell>,
    theme: &AppTheme,
) -> Line<'static> {
    match source {
        DiffVisualSource::Plain {
            row, byte_range, ..
        } => {
            let comment_count = row_comment_count(session, &rows[*row]);
            let spans = unified_row_line(session, &rows[*row], *row, comment_count, theme).spans;
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
            theme,
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
                        theme,
                    )
                },
            );
            spans.push(Span::styled("\u{2502}", Style::default().fg(theme.muted)));
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
                        theme,
                    )
                },
            ));
            Line::from(spans)
        }
        DiffVisualSource::Annotation {
            owner,
            card,
            layout,
            line,
            ..
        } => layout.line(
            *line,
            card.channel,
            selected_annotation.map_or(
                session.focus == Focus::Diff && render_cursor(session) == *owner,
                |selected| selected == &card.source,
            ),
            render_row_in_range(session, *owner),
            theme,
        ),
    }
}

impl DiffVisualSource {
    fn width(&self) -> usize {
        match self {
            Self::Plain { width, .. } | Self::Annotation { width, .. } => *width,
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
    theme: &AppTheme,
) -> Vec<Span<'static>> {
    let prepared = prepared
        .entry((cell.row, cell.lineno))
        .or_insert_with(|| prepare_diff_cell(session, rows, line_number_width, cell, theme));
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
    theme: &AppTheme,
) -> PreparedDiffCell {
    const CHROME_SPANS: usize = 5;
    let row = &rows[cell.row];
    let comments = row_comment_count(session, row);
    let mut all = diff_line_cell_spans_with_width(
        session,
        row,
        cell.row,
        cell.lineno,
        comments,
        line_number_width,
        theme,
    );
    let content = if all.len() > CHROME_SPANS {
        all.split_off(CHROME_SPANS)
    } else {
        Vec::new()
    };
    let continuation_style = diff_row_style(
        row.kind,
        session.focus == Focus::Diff && render_cursor(session) == cell.row,
        render_row_in_range(session, cell.row),
        theme,
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

pub(super) fn diff_row_at_point(
    session: &ReviewSession,
    inner: Rect,
    x: u16,
    visible_row: usize,
    tui_state: &TuiState,
) -> Option<usize> {
    tui_state
        .diff_viewport
        .row_at_point(session, inner, x, visible_row)
}

pub(super) fn diff_hit_at_point(
    session: &ReviewSession,
    inner: Rect,
    x: u16,
    visible_row: usize,
    tui_state: &TuiState,
) -> Option<DiffPointHit> {
    tui_state
        .diff_viewport
        .hit_at_point(session, inner, x, visible_row)
}

pub(super) fn scroll_diff_visual(
    session: &mut ReviewSession,
    inner: Rect,
    delta: isize,
    tui_state: &TuiState,
) {
    tui_state.diff_viewport.visual_scroll(session, inner, delta);
}

pub(super) fn scroll_diff_horizontal_visual(
    session: &mut ReviewSession,
    inner: Rect,
    delta: isize,
    tui_state: &TuiState,
) {
    tui_state
        .diff_viewport
        .horizontal_scroll(session, inner, delta);
}

pub(super) fn scroll_diff_to_bottom_visual(
    session: &mut ReviewSession,
    inner: Rect,
    tui_state: &TuiState,
) {
    tui_state.diff_viewport.scroll_to_bottom(session, inner);
}

pub(super) fn ensure_diff_cursor_visible(
    session: &mut ReviewSession,
    inner: Rect,
    tui_state: &TuiState,
) {
    tui_state.diff_viewport.logical_selection(session, inner);
}

#[cfg(test)]
pub(super) fn reconcile_diff_viewport(
    session: &mut ReviewSession,
    inner: Rect,
    keep_cursor_visible: bool,
    tui_state: &TuiState,
) {
    tui_state
        .diff_viewport
        .reflow(session, inner, keep_cursor_visible);
}

#[cfg(test)]
pub(super) fn diff_cursor_is_visible(
    session: &ReviewSession,
    inner: Rect,
    tui_state: &TuiState,
) -> bool {
    tui_state.diff_viewport.cursor_is_visible(session, inner)
}

/// One full-width line for a diff row in the unified layout (also used for
/// full-width rows in the side-by-side layout).
fn unified_row_line(
    session: &ReviewSession,
    row: &DiffRow,
    index: usize,
    comment_count: usize,
    theme: &AppTheme,
) -> Line<'static> {
    match row.kind {
        DiffRowKind::ChapterHeader => Line::from(Span::styled(
            row.text.clone(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        DiffRowKind::SkimFold => Line::from(Span::styled(
            row.text.clone(),
            Style::default()
                .fg(theme.secondary)
                .add_modifier(Modifier::ITALIC),
        )),
        DiffRowKind::FileHeader => Line::from(Span::styled(
            row.text.clone(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        DiffRowKind::SyntaxSummary => Line::from(Span::styled(
            row.text.clone(),
            Style::default().fg(theme.secondary),
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
            Style::default().fg(theme.info).add_modifier(Modifier::BOLD),
        )),
        DiffRowKind::Raw => Line::from(row.text.clone()),
        DiffRowKind::Placeholder => Line::from(Span::styled(
            format!("  \u{2298} {}", row.text),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        )),
        DiffRowKind::ContextFold | DiffRowKind::ExpandGap { .. } => Line::from(Span::styled(
            format!("      {}", row.text),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        )),
        DiffRowKind::DiffLine(_) => Line::from(diff_line_cell_spans(
            session,
            row,
            index,
            row.new_lineno.or(row.old_lineno),
            comment_count,
            theme,
        )),
    }
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
    theme: &AppTheme,
) -> Vec<Span<'static>> {
    diff_line_cell_spans_with_width(session, row, index, lineno, comment_count, 4, theme)
}

fn diff_line_cell_spans_with_width(
    session: &ReviewSession,
    row: &DiffRow,
    index: usize,
    lineno: Option<usize>,
    comment_count: usize,
    line_number_width: usize,
    theme: &AppTheme,
) -> Vec<Span<'static>> {
    let DiffRowKind::DiffLine(line_kind) = row.kind else {
        return vec![Span::raw(row.text.clone())];
    };
    let selected = session.focus == Focus::Diff && render_cursor(session) == index;
    let in_range = render_row_in_range(session, index);
    let style = diff_row_style(row.kind, selected, in_range, theme);
    let flagged = if session.stream_mode {
        session.stream_row_flagged(index, row)
    } else {
        session.diff_row_flagged(row)
    };
    let cues = &session.diff_cues;
    let (comment_mark, mark_style) = if comment_count > 0 {
        (
            match comment_count {
                1..=9 => comment_count.to_string(),
                _ => "+".to_owned(),
            },
            Style::default().fg(row_comment_channel(session, row)
                .map(|channel| theme.channel_color(channel))
                .unwrap_or(theme.muted)),
        )
    } else if flagged {
        // Agent-flagged section: pinned in the gutter.
        (
            "!".to_owned(),
            Style::default()
                .fg(theme.negative)
                .add_modifier(Modifier::BOLD),
        )
    } else if in_range {
        ("|".to_owned(), Style::default().fg(theme.accent))
    } else if cues.gutter_bar && matches!(line_kind, DiffLineKind::Added | DiffLineKind::Removed) {
        // The bar fills otherwise-empty gutter cells on changed lines.
        match line_kind {
            DiffLineKind::Added => (
                "\u{258e}".to_owned(),
                style_cue(cues.theme.gutter_added.as_deref(), theme.gutter_added),
            ),
            _ => (
                "\u{258e}".to_owned(),
                style_cue(cues.theme.gutter_removed.as_deref(), theme.gutter_removed),
            ),
        }
    } else {
        (" ".to_owned(), Style::default().fg(theme.accent))
    };

    // Cursor and range-selection backgrounds win over the cue backgrounds.
    let plain = !selected && !in_range;
    let line_bg = (plain && cues.line_background)
        .then(|| match line_kind {
            DiffLineKind::Added => {
                line_bg_cue(cues.theme.added_line_bg.as_deref(), theme.added_line_bg)
            }
            DiffLineKind::Removed => {
                line_bg_cue(cues.theme.removed_line_bg.as_deref(), theme.removed_line_bg)
            }
            _ => None,
        })
        .flatten();
    let emphasis_style = (plain && cues.word_highlight && !row.emphasis.is_empty())
        .then(|| match line_kind {
            DiffLineKind::Added => Some(style_cue(
                cues.theme.added_word.as_deref(),
                theme.added_word,
            )),
            DiffLineKind::Removed => Some(style_cue(
                cues.theme.removed_word.as_deref(),
                theme.removed_word,
            )),
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
        Span::styled(lineno, Style::default().fg(theme.muted)),
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
        theme,
    ));
    spans
}

/// Diff pane title: just "diff" normally; with the file pane hidden it
/// carries the selected file path and viewed mark so context is never lost.
fn diff_pane_title(session: &ReviewSession, file_pane_visible: bool) -> String {
    if file_pane_visible {
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

fn comment_badge_spans(comment: &Comment, theme: &AppTheme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if let Some(action) = comment.action
        && action != crate::state::ActionIntent::None
    {
        spans.push(Span::styled(
            format!("[{}] ", action_intent_label(action)),
            Style::default().fg(theme.secondary),
        ));
    }
    if let Some(kind) = comment.kind {
        spans.push(Span::styled(
            format!("[{}] ", comment_kind_label(kind)),
            Style::default().fg(theme.info),
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

fn comment_state_style(state: crate::state::CommentState, theme: &AppTheme) -> Style {
    match state {
        crate::state::CommentState::Draft => Style::default().fg(theme.muted),
        crate::state::CommentState::Todo => Style::default().fg(theme.negative),
        crate::state::CommentState::Resolved => Style::default().fg(theme.positive),
    }
}

#[allow(clippy::too_many_arguments)]
fn diff_text_spans(
    row: &DiffRow,
    fallback_style: Style,
    selected: bool,
    in_range: bool,
    line_bg: Option<Color>,
    emphasis_style: Option<Style>,
    theme: &SyntaxThemeConfig,
    chrome: &AppTheme,
) -> Vec<Span<'static>> {
    let mut segments: Vec<(String, Style)> = if row.syntax.is_empty() {
        vec![(row.text.clone(), fallback_style)]
    } else {
        row.syntax
            .iter()
            .map(|span| {
                let mut style = syntax_span_style(span, selected, in_range, theme, chrome);
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
    chrome: &AppTheme,
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
        style.bg(chrome.selection_bg).add_modifier(Modifier::BOLD)
    } else if in_range {
        style.bg(chrome.range_bg)
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

/// Line-background cue: an explicitly configured spec is literal (bare
/// colors read as backgrounds, `on <color>` respected, modifier-only specs
/// contribute nothing); an unset field derives from the [`AppTheme`].
fn line_bg_cue(spec: Option<&str>, derived: Color) -> Option<Color> {
    match spec {
        Some(spec) => spec_bg_color(spec),
        None => Some(derived),
    }
}

/// Word-emphasis / gutter cue: an explicitly configured spec is literal
/// (bare colors are foregrounds, only `on <color>` sets a background,
/// modifier-only specs add no colors); an unset field derives from the
/// [`AppTheme`].
fn style_cue(spec: Option<&str>, derived: Style) -> Style {
    match spec {
        Some(spec) => syntax_style_spec(spec),
        None => derived,
    }
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

/// Rewrite `#rrggbb` tokens in explicitly configured diff cue specs to
/// their nearest xterm-256 indexed colors, for terminals without truecolor
/// support (docs/focused-diff-ux.md §1). Named and indexed tokens pass
/// through; unset (derived) entries quantize inside [`AppTheme`] instead.
pub(super) fn downgrade_diff_theme(theme: &mut crate::config::DiffThemeConfig) {
    for spec in [
        &mut theme.added_line_bg,
        &mut theme.removed_line_bg,
        &mut theme.added_word,
        &mut theme.removed_word,
        &mut theme.gutter_added,
        &mut theme.gutter_removed,
    ]
    .into_iter()
    .flatten()
    {
        *spec = quantize_spec(spec);
    }
}

fn quantize_spec(spec: &str) -> String {
    spec.split_whitespace()
        .map(|token| match token.strip_prefix('#') {
            Some(hex) if hex.len() == 6 => match u32::from_str_radix(hex, 16) {
                Ok(value) => super::theme::nearest_indexed(super::theme::Rgb::new(
                    (value >> 16) as u8,
                    (value >> 8) as u8,
                    value as u8,
                ))
                .to_string(),
                Err(_) => token.to_owned(),
            },
            _ => token.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn diff_row_style(kind: DiffRowKind, selected: bool, in_range: bool, theme: &AppTheme) -> Style {
    let style = match kind {
        DiffRowKind::DiffLine(DiffLineKind::Context) => Style::default().fg(theme.subtle),
        DiffRowKind::DiffLine(DiffLineKind::Added) => Style::default().fg(theme.positive),
        DiffRowKind::DiffLine(DiffLineKind::Removed) => Style::default().fg(theme.negative),
        DiffRowKind::DiffLine(DiffLineKind::Meta) => Style::default().fg(theme.muted),
        _ => Style::default(),
    };

    if selected {
        style.bg(theme.selection_bg).add_modifier(Modifier::BOLD)
    } else if in_range {
        style.bg(theme.range_bg)
    } else {
        style
    }
}

fn draw_footer(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    session: &ReviewSession,
    context: &FooterContext<'_>,
    theme: &AppTheme,
) {
    let mode = context.mode;
    let keymap = context.keymap;
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
        Mode::Normal => {
            let mut text = footer_line(diff_footer_segments(
                session,
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
            if context.attention_focus {
                text = format!("Focus preset · {text}");
            }
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
        Mode::CommentInput { editor, target } => format!(
            "{kind} comment → {audience} · refresh paused · {channel} channel · {newline} newline · {submit} save · {cancel} cancel",
            kind = match target {
                CommentInputTarget::New => "new",
                CommentInputTarget::NewGeneral => "new general",
                CommentInputTarget::Edit { .. } => "edit",
                CommentInputTarget::AcceptDraft { .. } => "accept draft",
            },
            audience = editor.channel.audience_label(),
            channel = keymap.hint(Action::CycleCommentChannel),
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
        Mode::AttentionGlance(_) => format!(
            "attention glance · refresh paused · {down}/{up} move · {jump} jump · {peek} peek · {ack} acknowledge · {all} acknowledge all · {close} close",
            down = keymap.hint(Action::PopupMoveDown),
            up = keymap.hint(Action::PopupMoveUp),
            jump = keymap.hint(Action::PopupSelect),
            peek = keymap.hint(Action::GlancePeek),
            ack = keymap.hint(Action::GlanceAcknowledge),
            all = keymap.hint(Action::GlanceAcknowledgeAll),
            close = keymap.hint(Action::PopupClose),
        ),
        Mode::Help => format!(
            "help · {}/{} or page keys scroll · {}/{}/{} close",
            keymap.hint(Action::PopupMoveDown),
            keymap.hint(Action::PopupMoveUp),
            keymap.hint(Action::PopupClose),
            keymap.hint(Action::PopupCloseQ),
            keymap.hint(Action::Help),
        ),
    };
    let mut summary = if session.stream_mode {
        session.coverage_summary_line()
    } else {
        session.summary_line()
    };
    let display_target = &session.target;
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
            UiNoticeLevel::Info => ("info", Style::default().fg(theme.info)),
            UiNoticeLevel::Error => ("error", Style::default().fg(theme.negative)),
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
        Paragraph::new(lines).style(Style::default().fg(theme.muted)),
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
    let target = session.target.to_string();
    let mut segments = vec![target, focus_label.to_owned()];
    segments.extend(hint_segments(keymap, &hints));
    segments
}

fn diff_footer_segments(
    session: &ReviewSession,
    keymap: &KeyMap,
    focus_label: &str,
) -> Vec<String> {
    let hints = if session.stream_mode {
        vec![
            FooterHint::new([Action::MoveDown, Action::MoveUp], "line"),
            FooterHint::new(
                [Action::SpotlightNext, Action::SpotlightPrevious],
                "spotlight",
            ),
            FooterHint::new(
                [Action::AttentionPromote, Action::AttentionDemote],
                "salience",
            ),
            FooterHint::new([Action::AttentionFocus], "Focus"),
            FooterHint::new([Action::AttentionGlance], "glance"),
            FooterHint::new([Action::ToggleFold], "peek fold"),
            FooterHint::new([Action::MarkAllViewed], "ack fold"),
            FooterHint::new([Action::ScrollDown, Action::ScrollUp], "scroll"),
            FooterHint::new([Action::ViewOptions], "view/wrap"),
            FooterHint::new([Action::Comment], "comment"),
            FooterHint::new([Action::YankHandoff], "handoff"),
            FooterHint::new([Action::ToggleFocus], "files"),
            FooterHint::new([Action::Help], "help"),
            FooterHint::new([Action::Quit], "quit"),
        ]
    } else {
        vec![
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
        ]
    };
    let target = session.target.to_string();
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
fn draw_help_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    keymap: &KeyMap,
    scroll: usize,
    theme: &AppTheme,
) {
    let popup = centered_rect(90, 80, area);
    clear_popup(frame, popup, theme);

    let section = |title: &str| {
        Line::from(Span::styled(
            title.to_owned(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let entry = |actions: &[Action], label: &str| {
        let keys: Vec<&str> = actions.iter().map(|action| keymap.hint(*action)).collect();
        Line::from(vec![
            Span::styled(
                format!("  {:>10}  ", keys.join("/")),
                Style::default().fg(theme.detail),
            ),
            Span::styled(label.to_owned(), Style::default().fg(theme.subtle)),
        ])
    };
    let literal = |keys: &str, label: &str| {
        Line::from(vec![
            Span::styled(
                format!("  {:>10}  ", keys),
                Style::default().fg(theme.detail),
            ),
            Span::styled(label.to_owned(), Style::default().fg(theme.subtle)),
        ])
    };

    let left = vec![
        section("first review loop"),
        entry(&[Action::FileSearch], "open a file or fuzzy jump"),
        entry(&[Action::NextUnviewed], "next unviewed file"),
        entry(
            &[Action::NextFile, Action::PreviousFile],
            "next/previous file from either pane",
        ),
        entry(&[Action::MarkViewed], "mark viewed and advance"),
        entry(&[Action::Comment], "comment on what needs work"),
        entry(&[Action::AttentionFocus], "toggle attention Focus preset"),
        entry(&[Action::AttentionGlance], "open attention glance board"),
        entry(&[Action::YankHandoff], "copy handoff when done"),
        section("diff & view"),
        entry(&[Action::MoveDown, Action::MoveUp], "move diff cursor"),
        entry(&[Action::ScrollDown, Action::ScrollUp], "vertical scroll"),
        entry(
            &[Action::ScrollDiffLeft, Action::ScrollDiffRight],
            "horizontal scroll when wrap is off",
        ),
        entry(&[Action::ViewOptions], "soft wrap, layout, and visual cues"),
        entry(
            &[Action::WidenFilePane, Action::NarrowFilePane],
            "widen/narrow file pane split",
        ),
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
        entry(
            &[Action::SpotlightNext, Action::SpotlightPrevious],
            "next/previous spotlight in the review stream",
        ),
        entry(
            &[Action::AttentionPromote, Action::AttentionDemote],
            "promote/demote region salience",
        ),
        entry(&[Action::ToggleFold], "peek/collapse selected skim fold"),
        entry(
            &[Action::MarkAllViewed],
            "ack selected skim fold (no-op on ordinary stream rows)",
        ),
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
            &[Action::ToggleAnnotationArtifacts],
            "expand/collapse inline card artifacts",
        ),
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
        section("attention views"),
        entry(&[Action::AttentionFocus], "toggle Focus preset"),
        entry(&[Action::AttentionGlance], "open glance board"),
        entry(&[Action::GlancePeek], "peek selected skim fold"),
        entry(&[Action::GlanceAcknowledge], "acknowledge selected skim"),
        entry(
            &[Action::GlanceAcknowledgeAll],
            "acknowledge all current skims",
        ),
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
        entry(
            &[Action::CycleCommentChannel],
            "cycle comment channel while composing",
        ),
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
    theme: &AppTheme,
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
    clear_popup(frame, popup, theme);
    let title = comment_popup_title(
        session,
        target,
        editor.channel,
        popup.width.saturating_sub(4) as usize,
    );
    let hint = format!(
        "{} channel · {} save · {} cancel",
        keymap.hint(Action::CycleCommentChannel),
        keymap.hint(Action::SubmitComment),
        keymap.hint(Action::CancelComment)
    );
    frame.render_widget(
        Paragraph::new(rows).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.channel_color(editor.channel)))
                .title(Line::from(Span::styled(
                    title,
                    Style::default().fg(theme.channel_color(editor.channel)),
                )))
                .title_bottom(Line::from(Span::styled(
                    hint,
                    Style::default().fg(theme.muted),
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
    channel: Channel,
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
    truncate_middle(
        &format!("{kind} → {} · {location}", channel.audience_label()),
        max_width,
    )
}

fn draw_revset_input_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    input: &RevsetInputState,
    keymap: &KeyMap,
    theme: &AppTheme,
) {
    let popup = centered_rect(70, 30, area);
    clear_popup(frame, popup, theme);

    let field_line = |label: &str, value: &str, active: bool| {
        let marker = if active { "›" } else { " " };
        let value_style = if active {
            Style::default().fg(theme.foreground)
        } else {
            Style::default().fg(theme.subtle)
        };
        Line::from(vec![
            Span::styled(
                format!("{marker} {label:>4}: "),
                if active {
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.muted)
                },
            ),
            Span::styled(value.to_owned(), value_style),
        ])
    };

    let lines = vec![
        Line::from(Span::styled(
            "Review an arbitrary revset range (jj revset syntax)",
            Style::default().fg(theme.muted),
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
            Style::default().fg(theme.muted),
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
    theme: &AppTheme,
) {
    let popup = centered_rect(76, 46, area);
    clear_popup(frame, popup, theme);

    let mut lines = Vec::new();
    if state.confirming {
        let command = state
            .selected_option()
            .map(JjHelperOption::command_line)
            .unwrap_or_default();
        lines.push(Line::from(Span::styled(
            "About to run:",
            Style::default().fg(theme.muted),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {command}"),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "This rewrites history in your repo. {} run · {} back",
                keymap.hint(Action::PopupSelect),
                keymap.hint(Action::PopupClose)
            ),
            Style::default().fg(theme.negative),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "jj helpers (nothing runs until you confirm)",
            Style::default().fg(theme.muted),
        )));
        lines.push(Line::from(""));
        for (index, option) in state.options.iter().enumerate() {
            let selected = index == state.selected;
            let marker = if selected { "›" } else { " " };
            let style = if selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.subtle)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{marker} {}", option.label), style),
                Span::styled(
                    format!("  ({})", option.command_line()),
                    Style::default().fg(theme.muted),
                ),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            list_popup_hint("", keymap, "select")
                .trim_start_matches(" · ")
                .to_owned(),
            Style::default().fg(theme.muted),
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
    theme: &AppTheme,
    file_pane_visible: bool,
) {
    let popup = centered_rect(56, 70, area);
    clear_popup(frame, popup, theme);

    let mut lines = vec![Line::from(Span::styled(
        "Diff visual cues (session only; gander.toml [diff] sets defaults)",
        Style::default().fg(theme.muted),
    ))];
    lines.push(Line::from(""));
    for (index, option) in ViewOption::ALL.into_iter().enumerate() {
        let selected = index == state.selected;
        let marker = if selected { "›" } else { " " };
        let checkbox = if option.enabled(session, file_pane_visible) {
            "[x]"
        } else {
            "[ ]"
        };
        let style = if selected {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.subtle)
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
        Style::default().fg(theme.muted),
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
    theme: &AppTheme,
) {
    let popup = centered_rect(80, 60, area);
    clear_popup(frame, popup, theme);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 4usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window = picker_visible_window(list.selected, list.flags.len(), list_height);

    let mut lines = vec![Line::from(Span::styled(
        "Sections flagged by agents, critical first",
        Style::default().fg(theme.muted),
    ))];
    if list.flags.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no flags",
            Style::default().fg(theme.muted),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(theme.muted),
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
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.subtle)
                    };
                    let location = match flag.line {
                        Some(line) => format!("{}:{line}", flag.path),
                        None => flag.path.clone(),
                    };
                    Line::from(vec![
                        Span::styled(format!("{marker} "), style),
                        Span::styled(
                            format!("[{:^8}] ", flag.priority.label()),
                            flag_priority_style(flag.priority, theme),
                        ),
                        Span::styled(format!("{location} "), Style::default().fg(theme.detail)),
                        Span::styled(flag.reason.clone(), style),
                    ])
                }),
        );
        if visible_window.hidden_below > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more", visible_window.hidden_below),
                Style::default().fg(theme.muted),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        list_popup_hint("", keymap, "jump")
            .trim_start_matches(" · ")
            .to_owned(),
        Style::default().fg(theme.muted),
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
    let left: String = text
        .chars()
        .scan(0, |width, ch| {
            let next = *width + ch.width().unwrap_or(0);
            (next <= left_w).then(|| {
                *width = next;
                ch
            })
        })
        .collect();
    let mut right = Vec::new();
    let mut width = 0;
    for ch in text.chars().rev() {
        let next = width + ch.width().unwrap_or(0);
        if next > right_w {
            break;
        }
        right.push(ch);
        width = next;
    }
    format!("{left}…{}", right.into_iter().rev().collect::<String>())
}

fn draw_draft_list_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    list: &DraftListState,
    keymap: &KeyMap,
    theme: &AppTheme,
) {
    let popup = centered_rect(82, 60, area);
    clear_popup(frame, popup, theme);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 3usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window = picker_visible_window(list.selected, list.drafts.len(), list_height);

    let mut lines = vec![Line::from(Span::styled(
        "Agent-drafted comments awaiting your decision",
        Style::default().fg(theme.muted),
    ))];
    if list.drafts.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no pending drafts",
            Style::default().fg(theme.muted),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(theme.muted),
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
                    let card = AnnotationCard::from_comment(draft);
                    let mut spans = vec![Span::styled(
                        format!("{marker} "),
                        Style::default().fg(theme.channel_color(draft.channel)),
                    )];
                    spans.extend(card.compact_line(selected, theme).spans);
                    Line::from(spans)
                }),
        );
        if visible_window.hidden_below > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more", visible_window.hidden_below),
                Style::default().fg(theme.muted),
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
        Style::default().fg(theme.muted),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("agent drafts"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn flag_priority_style(priority: crate::agent::FlagPriority, theme: &AppTheme) -> Style {
    match priority {
        crate::agent::FlagPriority::Critical => Style::default()
            .fg(theme.negative)
            .add_modifier(Modifier::BOLD),
        crate::agent::FlagPriority::High => Style::default().fg(theme.negative),
        crate::agent::FlagPriority::Medium => Style::default().fg(theme.warning),
        crate::agent::FlagPriority::Low => Style::default().fg(theme.muted),
    }
}

fn draw_operation_picker_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    picker: &OperationPickerState,
    keymap: &KeyMap,
    theme: &AppTheme,
) {
    let popup = centered_rect(80, 60, area);
    clear_popup(frame, popup, theme);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 3usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window =
        picker_visible_window(picker.selected, picker.operations.len(), list_height);

    let mut lines = vec![Line::from(Span::styled(
        "Compare against a prior operation: unchanged files are marked caught up",
        Style::default().fg(theme.muted),
    ))];
    if picker.operations.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no operations",
            Style::default().fg(theme.muted),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(theme.muted),
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
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.subtle)
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
                            Style::default().fg(theme.detail),
                        ),
                        Span::styled(description, style),
                    ])
                }),
        );
        if visible_window.hidden_below > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more", visible_window.hidden_below),
                Style::default().fg(theme.muted),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        picker
            .preview
            .as_deref()
            .unwrap_or("select an operation to preview catch-up"),
        Style::default().fg(theme.accent),
    )));
    lines.push(Line::from(Span::styled(
        format!(
            "{}/{} move · {} apply · {} cancel",
            keymap.hint(Action::PopupMoveDown),
            keymap.hint(Action::PopupMoveUp),
            keymap.hint(Action::PopupSelect),
            keymap.hint(Action::PopupClose),
        ),
        Style::default().fg(theme.muted),
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
    theme: &AppTheme,
) {
    let popup = centered_rect(72, 60, area);
    clear_popup(frame, popup, theme);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 3usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window = picker_visible_window(search.selected, search.filtered.len(), list_height);

    let mut lines = vec![Line::from(vec![
        Span::styled("search: ", Style::default().fg(theme.muted)),
        Span::styled(
            if search.query.is_empty() {
                "type to fuzzy match files".to_owned()
            } else {
                search.query.clone()
            },
            if search.query.is_empty() {
                Style::default().fg(theme.muted)
            } else {
                Style::default().fg(theme.foreground)
            },
        ),
    ])];
    if search.filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matching files",
            Style::default().fg(theme.muted),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(theme.muted),
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
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD)
                    } else if row.viewed {
                        Style::default().fg(theme.muted)
                    } else {
                        Style::default().fg(theme.subtle)
                    };
                    Line::from(vec![
                        Span::styled(format!("{marker} "), style),
                        Span::styled(viewed_mark, Style::default().fg(theme.positive)),
                        Span::styled(format!(" {}", row.path), style),
                    ])
                }),
        );
        if visible_window.hidden_below > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more", visible_window.hidden_below),
                Style::default().fg(theme.muted),
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
        Style::default().fg(theme.muted),
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
    theme: &AppTheme,
) {
    let popup = centered_rect(60, 50, area);
    clear_popup(frame, popup, theme);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 2usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window =
        picker_visible_window(outline.selected, outline.targets.len(), list_height);

    let mut lines = Vec::new();
    if outline.targets.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no changed symbols",
            Style::default().fg(theme.muted),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(theme.muted),
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
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.subtle)
                    };
                    Line::from(Span::styled(format!("{marker} {}", target.label), style))
                }),
        );
        if visible_window.hidden_below > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more", visible_window.hidden_below),
                Style::default().fg(theme.muted),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        list_popup_hint("", keymap, "jump")
            .trim_start_matches(" · ")
            .to_owned(),
        Style::default().fg(theme.muted),
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
    theme: &AppTheme,
) {
    let popup = centered_rect(80, 60, area);
    clear_popup(frame, popup, theme);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 2usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window = picker_visible_window(list.selected, session.comments.len(), list_height);

    let mut lines = Vec::new();
    if session.comments.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no comments",
            Style::default().fg(theme.muted),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(theme.muted),
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
                    let card = AnnotationCard::from_comment(comment);
                    let mut spans = vec![Span::styled(
                        format!("{marker} "),
                        Style::default().fg(theme.channel_color(comment.channel)),
                    )];
                    spans.extend(card.compact_line(selected, theme).spans);
                    Line::from(spans)
                }),
        );
        if visible_window.hidden_below > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more", visible_window.hidden_below),
                Style::default().fg(theme.muted),
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
        Style::default().fg(theme.muted),
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
    theme: &AppTheme,
) {
    let popup = centered_rect(82, 60, area);
    clear_popup(frame, popup, theme);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 2usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window = picker_visible_window(list.selected, list.rows.len(), list_height);

    let mut lines = Vec::new();
    if list.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no open action items or todo feedback",
            Style::default().fg(theme.muted),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(theme.muted),
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
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.subtle)
            };
            lines.push(open_work_line(session, row, marker, style, theme));
        }
        if visible_window.hidden_below > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more", visible_window.hidden_below),
                Style::default().fg(theme.muted),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        list_popup_hint("", keymap, "jump")
            .trim_start_matches(" · ")
            .to_owned(),
        Style::default().fg(theme.muted),
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
    theme: &AppTheme,
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
                Span::styled("[item] ", Style::default().fg(theme.accent)),
            ];
            if let Some(action) = action
                && *action != crate::state::ActionIntent::None
            {
                spans.push(Span::styled(
                    format!("[{}] ", action_intent_label(*action)),
                    Style::default().fg(theme.secondary),
                ));
            }
            spans.extend([
                Span::styled(
                    format!("{} ", action_item_location(target.as_ref().as_ref())),
                    Style::default().fg(theme.detail),
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
                    Style::default().fg(theme.muted),
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
                    Style::default().fg(theme.channel_color(comment.channel)),
                ),
                Span::styled(
                    format!("[→ {}] ", comment.channel.audience_label()),
                    Style::default().fg(theme.channel_color(comment.channel)),
                ),
                Span::styled(
                    format!("{} ", comment_list_location(comment)),
                    Style::default().fg(theme.detail),
                ),
                Span::styled(
                    format!("[{}] ", comment.state.label()),
                    comment_state_style(comment.state, theme),
                ),
            ];
            spans.extend(comment_badge_spans(comment, theme));
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
    let theme = &tui_state.theme;
    let popup = centered_rect(72, 60, area);
    clear_popup(frame, popup, theme);
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
                Span::styled(time, Style::default().fg(theme.muted)),
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
            .fg(theme.accent)
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
            .style(Style::default().fg(theme.subtle))
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
    theme: &AppTheme,
) {
    let popup = centered_rect(84, 60, area);
    clear_popup(frame, popup, theme);
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
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.subtle)
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
            Span::styled(format!("{location} "), Style::default().fg(theme.detail)),
            Span::styled(why.to_owned(), Style::default().fg(theme.muted)),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no walkthrough steps",
            Style::default().fg(theme.muted),
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
        Style::default().fg(theme.muted),
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
    theme: &AppTheme,
) {
    let popup = centered_rect(84, 64, area);
    clear_popup(frame, popup, theme);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let fixed_lines = 6usize;
    let list_height = inner_height.saturating_sub(fixed_lines).max(1);
    let visible_window =
        picker_visible_window(chooser.selected, chooser.filtered.len(), list_height);

    let mut lines = vec![
        Line::from(vec![
            Span::raw("Choose "),
            Span::styled(chooser.selecting.label(), Style::default().fg(theme.accent)),
            Span::raw(" for "),
            Span::styled(
                format!("{}..{}", chooser.current_base, chooser.current_tip),
                Style::default().fg(theme.accent),
            ),
            Span::styled(" (tab toggles base/tip)", Style::default().fg(theme.muted)),
        ]),
        Line::from(vec![
            Span::styled("filter: ", Style::default().fg(theme.muted)),
            Span::styled(
                if chooser.query.is_empty() {
                    "type to fuzzy match".to_owned()
                } else {
                    chooser.query.clone()
                },
                if chooser.query.is_empty() {
                    Style::default().fg(theme.muted)
                } else {
                    Style::default().fg(theme.foreground)
                },
            ),
        ]),
        Line::from(Span::styled(
            "   change id      bookmarks                 description",
            Style::default().fg(theme.muted),
        )),
    ];
    if chooser.filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matching changes",
            Style::default().fg(theme.muted),
        )));
    } else {
        if visible_window.hidden_above > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} more", visible_window.hidden_above),
                Style::default().fg(theme.muted),
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
                        theme,
                    )
                }),
        );
        if visible_window.hidden_below > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more", visible_window.hidden_below),
                Style::default().fg(theme.muted),
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
        Style::default().fg(theme.muted),
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
    theme: &AppTheme,
) -> Line<'static> {
    let style = if selected {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.subtle)
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
            Style::default().fg(theme.detail),
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
fn measured_content_width(available: usize, requested: usize) -> usize {
    requested.clamp(72.min(available), 100.min(available))
}

#[cfg(test)]
fn render_tui_text(session: &ReviewSession, mode: &Mode, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    let keymap = KeyMap::try_from(&crate::config::KeybindingsConfig::default()).unwrap();
    let tui_state = TuiState {
        terminal_size: ratatui::prelude::Size::new(width, height),
        ..TuiState::default()
    };
    terminal
        .draw(|frame| draw(frame, session, mode, &keymap, &tui_state, None))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut output = (0..height)
        .map(|y| {
            let line = (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>();
            line.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n");
    output.push('\n');
    output
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
        state::{
            ActionIntent, AttentionRegion, AuthorKind, Channel, Comment, CommentKind, CommentReply,
            CommentState, Identity, Salience, SalienceSource, StepArtifact, StepArtifactKind,
            Walkthrough, WalkthroughStep,
        },
        syntax::{HighlightKind, SyntaxSpan, SyntaxThemeConfig},
        tui::test_support::snapshot_session,
    };

    #[test]
    fn tui_snapshot_salience_driven_cross_file_stream() {
        let mut session = snapshot_session(
            "diff --git a/a.gen.rs b/a.gen.rs\n--- a/a.gen.rs\n+++ b/a.gen.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/b.gen.rs b/b.gen.rs\n--- a/b.gen.rs\n+++ b/b.gen.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-old_main\n+new_main\n",
        );
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let spotlight =
            crate::attention::target_for_diff(&files, "src/main.rs", Some(1), None).unwrap();
        let mut durable = crate::state::ReviewSession {
            id: "stream-review".into(),
            attention_regions: vec![
                AttentionRegion {
                    target: crate::attention::target_for_diff(&files, "a.gen.rs", None, None)
                        .unwrap(),
                    salience: Salience::Skim,
                    rationale: Some("Skim: generated path policy".into()),
                    source: SalienceSource::Heuristic,
                },
                AttentionRegion {
                    target: crate::attention::target_for_diff(&files, "b.gen.rs", None, None)
                        .unwrap(),
                    salience: Salience::Skim,
                    rationale: Some("Skim: generated content marker".into()),
                    source: SalienceSource::Heuristic,
                },
                AttentionRegion {
                    target: spotlight.clone(),
                    salience: Salience::Spotlight,
                    rationale: Some("Establishes the entry point".into()),
                    source: SalienceSource::Agent,
                },
            ],
            walkthroughs: vec![Walkthrough {
                id: "walk".into(),
                steps: vec![WalkthroughStep {
                    id: "main".into(),
                    target: spotlight,
                    change_id: Some("abc123".into()),
                    title: Some("Start at main".into()),
                    why: Some("This wires the new behavior together.".into()),
                    body: Some("Follow the control flow into the supporting helpers.".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        durable.target.base = Some(session.target.base.clone());
        durable.target.revision = Some(session.target.rev.clone());
        durable.target.repo = Some(crate::review::canonical_repo_identity(&session.repo));
        session.sessions.push(durable);
        session.stack_changes.push(JjChangeSummary {
            change_id: "abc123".into(),
            bookmarks: "feature/stream".into(),
            description: "feat: continuous review stream".into(),
        });
        session.change_diffs.push((
            "abc123".into(),
            crate::diff::DiffSet::parse(
                "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-old_main\n+new_main\n",
            )
            .unwrap(),
        ));
        session.stream_mode = true;
        session.focus = Focus::Diff;
        session.stream_cursor = session
            .review_stream()
            .rows
            .iter()
            .position(|row| row.anchor.is_some())
            .unwrap();
        insta::assert_snapshot!(render_tui_text(&session, &Mode::Normal, 120, 28));
    }

    #[test]
    fn tui_snapshots_attention_glance_wide_and_narrow() {
        let mut session = snapshot_session(
            "diff --git a/a.gen.rs b/a.gen.rs\n--- a/a.gen.rs\n+++ b/a.gen.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/b.gen.rs b/b.gen.rs\n--- a/b.gen.rs\n+++ b/b.gen.rs\n@@ -1 +1 @@\n-old_b\n+new_b\n",
        );
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let mut stale = crate::attention::target_for_diff(&files, "b.gen.rs", None, None).unwrap();
        stale.anchor = None;
        let mut durable = crate::state::ReviewSession {
            id: "glance-review".into(),
            attention_regions: vec![
                AttentionRegion {
                    target: crate::attention::target_for_diff(&files, "a.gen.rs", None, None)
                        .unwrap(),
                    salience: Salience::Skim,
                    rationale: Some("Skim: generated path policy".into()),
                    source: SalienceSource::Heuristic,
                },
                AttentionRegion {
                    target: stale,
                    salience: Salience::Skim,
                    rationale: Some("old generated assignment".into()),
                    source: SalienceSource::Human,
                },
            ],
            ..Default::default()
        };
        durable.target.base = Some(session.target.base.clone());
        durable.target.revision = Some(session.target.rev.clone());
        durable.target.repo = Some(crate::review::canonical_repo_identity(&session.repo));
        session.sessions.push(durable);
        session.stream_mode = true;
        let mode = Mode::AttentionGlance(super::super::GlanceBoardState::new(&session));
        insta::assert_snapshot!(
            "tui_snapshot_attention_glance_wide",
            render_tui_text(&session, &mode, 120, 26)
        );
        insta::assert_snapshot!(
            "tui_snapshot_attention_glance_narrow",
            render_tui_text(&session, &mode, 54, 18)
        );
    }

    #[test]
    fn authoritative_file_pane_state_hides_and_splits_layout() {
        let config = UiConfig {
            file_pane_auto_hide_width: 80,
            file_pane_split_percent: 40,
            menu_bar: false,
        };
        let session = snapshot_session("");
        let mut tui_state = TuiState {
            layout_config: config,
            file_pane: super::super::FilePaneState {
                split_percent: 40,
                ..Default::default()
            },
            ..TuiState::default()
        };
        let narrow = tui_state.review_layout(&session, Rect::new(0, 0, 70, 20));
        assert_eq!(narrow.files.width, 0);
        assert_eq!(narrow.diff.width, 70);

        let wide = tui_state.review_layout(&session, Rect::new(0, 0, 140, 20));
        assert_eq!(wide.files.width, 56);
        assert_eq!(wide.diff.width, 84);

        tui_state.file_pane.split_percent = 20;
        let adjusted = tui_state.review_layout(&session, Rect::new(0, 0, 140, 20));
        assert_eq!(adjusted.files.width, 28);
        assert_eq!(adjusted.diff.width, 112);

        tui_state.file_pane.explicit_override = Some(true);
        let forced = tui_state.review_layout(&session, Rect::new(0, 0, 70, 20));
        assert!(forced.files.width > 0);
    }

    #[test]
    fn menu_bar_reserves_height_only_when_enabled_and_roomy() {
        let mut config = UiConfig {
            menu_bar: true,
            ..UiConfig::default()
        };
        assert_eq!(
            ui_layout(Rect::new(0, 0, 100, 20), true, &config, 30)
                .menu
                .height,
            1
        );
        assert_eq!(
            ui_layout(Rect::new(0, 0, 79, 20), true, &config, 30)
                .menu
                .height,
            0
        );
        assert_eq!(
            ui_layout(Rect::new(0, 0, 100, 17), true, &config, 30)
                .menu
                .height,
            0
        );
        config.menu_bar = false;
        assert_eq!(
            ui_layout(Rect::new(0, 0, 100, 20), true, &config, 30)
                .menu
                .height,
            0
        );
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
            .draw(|frame| draw(frame, session, mode, &keymap, &TuiState::default(), None))
            .unwrap();
        let position = terminal.backend().cursor_position();
        (
            terminal.backend().buffer().clone(),
            (position.x, position.y),
        )
    }

    fn render_comment_channel_snapshot(channel: Channel) -> String {
        let session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let mode = Mode::CommentInput {
            editor: CommentEditor::with_channel("channel-aware comment".into(), channel),
            target: CommentInputTarget::New,
        };
        let width = 90;
        let height = 20;
        let (buffer, _) = render_tui_buffer_and_cursor(&session, &mode, width, height);
        let popup = comment_popup_rect(Rect::new(0, 0, width, height));
        let border = buffer[(popup.x, popup.y)].style().fg;
        let title = buffer[(popup.x + 1, popup.y)].style().fg;
        format!(
            "{}\nstyles: border={border:?} title={title:?}",
            render_tui_text(&session, &mode, width, height)
        )
    }

    fn buffer_row(buffer: &Buffer, area: Rect, row: u16) -> String {
        (area.x..area.x + area.width)
            .map(|x| buffer[(x, area.y + row)].symbol())
            .collect::<String>()
    }

    fn style_runs_for_rows(buffer: &Buffer, needles: &[&str]) -> String {
        needles
            .iter()
            .map(|needle| {
                let area = buffer.area;
                let y = (area.y..area.y + area.height)
                    .find(|y| {
                        let row = (area.x..area.x + area.width)
                            .map(|x| buffer[(x, *y)].symbol())
                            .collect::<String>();
                        row.contains(needle)
                    })
                    .unwrap_or_else(|| panic!("missing style-run row {needle:?}"));
                let mut runs = Vec::new();
                let mut x = area.x;
                while x < area.x + area.width {
                    let style = buffer[(x, y)].style();
                    let start = x;
                    x += 1;
                    while x < area.x + area.width && buffer[(x, y)].style() == style {
                        x += 1;
                    }
                    let text = (start..x)
                        .map(|column| buffer[(column, y)].symbol())
                        .collect::<String>();
                    if !text.trim().is_empty() {
                        runs.push(format!("{text:?}={style:?}"));
                    }
                }
                format!("{needle}: {}", runs.join(" | "))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_tui_text_with_state(
        session: &ReviewSession,
        mode: &Mode,
        tui_state: &TuiState,
        width: u16,
        height: u16,
    ) -> String {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        render_tui_text_with_state_and_keymap(session, mode, tui_state, &keymap, width, height)
    }

    fn render_tui_buffer_with_state(
        session: &ReviewSession,
        mode: &Mode,
        tui_state: &TuiState,
        width: u16,
        height: u16,
    ) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        terminal
            .draw(|frame| draw(frame, session, mode, &keymap, tui_state, None))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn render_tui_text_with_state_and_keymap(
        session: &ReviewSession,
        mode: &Mode,
        tui_state: &TuiState,
        keymap: &KeyMap,
        width: u16,
        height: u16,
    ) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    session,
                    mode,
                    keymap,
                    tui_state,
                    tui_state.notice.as_ref(),
                )
            })
            .unwrap();
        buffer_text(terminal.backend().buffer())
    }

    fn menu_snapshot_state() -> TuiState {
        TuiState {
            layout_config: UiConfig {
                menu_bar: true,
                ..UiConfig::default()
            },
            ..TuiState::default()
        }
    }

    #[test]
    fn tui_snapshot_menu_with_gander_preset() {
        let session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        let rendered = render_tui_text_with_state_and_keymap(
            &session,
            &Mode::Normal,
            &menu_snapshot_state(),
            &keymap,
            110,
            20,
        );
        assert!(rendered.contains("? help  ] hunk  . file"), "{rendered}");
        insta::assert_snapshot!("tui_snapshot_menu_gander_preset", rendered);
    }

    #[test]
    fn tui_snapshot_menu_with_hunk_preset() {
        let session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let config = KeybindingsConfig::preset(crate::config::KeybindingPresetConfig::Hunk);
        let keymap = KeyMap::try_from(&config).unwrap();
        let rendered = render_tui_text_with_state_and_keymap(
            &session,
            &Mode::Normal,
            &menu_snapshot_state(),
            &keymap,
            110,
            20,
        );
        assert!(rendered.contains("alt-j hunk  alt-l file"), "{rendered}");
        insta::assert_snapshot!("tui_snapshot_menu_hunk_preset", rendered);
    }

    #[test]
    fn tui_snapshot_menu_with_custom_override() {
        let session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let config = KeybindingsConfig {
            help: vec!["alt-1".into()],
            next_changed_hunk: vec!["alt-2".into()],
            next_file: vec!["alt-3".into()],
            comment: vec!["alt-4".into()],
            view_options: vec!["alt-5".into()],
            toggle_file_pane: vec!["alt-6".into()],
            quit: vec!["alt-7".into()],
            ..KeybindingsConfig::default()
        };
        let keymap = KeyMap::try_from(&config).unwrap();
        let rendered = render_tui_text_with_state_and_keymap(
            &session,
            &Mode::Normal,
            &menu_snapshot_state(),
            &keymap,
            110,
            20,
        );
        assert!(
            rendered.contains("alt-1 help  alt-2 hunk  alt-3 file"),
            "{rendered}"
        );
        insta::assert_snapshot!("tui_snapshot_menu_custom_override", rendered);
    }

    #[test]
    fn tui_snapshot_menu_omits_unbound_item() {
        let session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let config = KeybindingsConfig {
            next_changed_hunk: Vec::new(),
            ..KeybindingsConfig::default()
        };
        let keymap = KeyMap::try_from(&config).unwrap();
        let rendered = render_tui_text_with_state_and_keymap(
            &session,
            &Mode::Normal,
            &menu_snapshot_state(),
            &keymap,
            110,
            20,
        );
        assert!(!rendered.lines().next().unwrap_or_default().contains("hunk"));
        insta::assert_snapshot!("tui_snapshot_menu_unbound_item_omitted", rendered);
    }

    #[test]
    fn tui_snapshot_menu_with_popup_and_footer() {
        let session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let config = KeybindingsConfig {
            popup_move_down: vec!["alt-j".into()],
            popup_move_up: vec!["alt-k".into()],
            comment_list_new_general: vec!["alt-g".into()],
            ..KeybindingsConfig::default()
        };
        let keymap = KeyMap::try_from(&config).unwrap();
        let rendered = render_tui_text_with_state_and_keymap(
            &session,
            &Mode::CommentList(CommentListState::default()),
            &menu_snapshot_state(),
            &keymap,
            120,
            22,
        );
        assert!(rendered.contains("alt-j/alt-k move"), "{rendered}");
        assert!(rendered.contains("alt-g general"), "{rendered}");
        insta::assert_snapshot!("tui_snapshot_menu_with_popup_and_footer", rendered);
    }

    #[test]
    fn tui_snapshot_file_pane_effective_layouts() {
        let session = snapshot_session(
            "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let wide = TuiState::default();
        insta::assert_snapshot!(
            "tui_snapshot_file_pane_wide_visible",
            render_tui_text_with_state(&session, &Mode::Normal, &wide, 100, 16)
        );

        let narrow = TuiState::default();
        let mut narrow_session = session.clone();
        narrow.correct_file_pane_focus(&mut narrow_session, 44);
        let narrow_rendered =
            render_tui_text_with_state(&narrow_session, &Mode::Normal, &narrow, 44, 16);
        assert!(
            narrow_rendered.contains("diff · src/a.rs"),
            "{narrow_rendered}"
        );
        insta::assert_snapshot!("tui_snapshot_file_pane_narrow_auto_hidden", narrow_rendered);

        let forced = TuiState {
            file_pane: super::super::FilePaneState {
                explicit_override: Some(true),
                ..Default::default()
            },
            ..TuiState::default()
        };
        insta::assert_snapshot!(
            "tui_snapshot_file_pane_narrow_forced_visible",
            render_tui_text_with_state(&session, &Mode::Normal, &forced, 44, 16)
        );

        let adjusted = TuiState {
            file_pane: super::super::FilePaneState {
                split_percent: 45,
                ..Default::default()
            },
            ..TuiState::default()
        };
        insta::assert_snapshot!(
            "tui_snapshot_file_pane_adjusted_split",
            render_tui_text_with_state(&session, &Mode::Normal, &adjusted, 100, 16)
        );
    }

    #[test]
    fn view_options_file_pane_checkbox_uses_effective_visibility() {
        let session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let mode = Mode::ViewOptions(ViewOptionsState { selected: 4 });
        let narrow = render_tui_text_with_state(&session, &mode, &TuiState::default(), 44, 32);
        assert!(narrow.contains("[ ] file pane"), "{narrow}");

        let forced = TuiState {
            file_pane: super::super::FilePaneState {
                explicit_override: Some(true),
                ..Default::default()
            },
            ..TuiState::default()
        };
        let forced = render_tui_text_with_state(&session, &mode, &forced, 44, 32);
        assert!(forced.contains("[x] file pane"), "{forced}");
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
            .draw(|frame| draw(frame, &session, &mode, &keymap, &TuiState::default(), None))
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
        render_tui_style_runs_with_state(session, mode, &TuiState::default(), width, height)
    }

    fn render_tui_style_runs_with_state(
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
        assert!(rendered.contains("next/previous file from either pane"));
        assert!(rendered.contains("widen/narrow file pane"));
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
    fn tui_snapshots_each_comment_editor_channel_border_and_chip() {
        insta::assert_snapshot!(
            "comment_editor_channel_onboarding",
            render_comment_channel_snapshot(Channel::Onboarding)
        );
        insta::assert_snapshot!(
            "comment_editor_channel_delegation",
            render_comment_channel_snapshot(Channel::Delegation)
        );
        insta::assert_snapshot!(
            "comment_editor_channel_collaboration",
            render_comment_channel_snapshot(Channel::Collaboration)
        );
        insta::assert_snapshot!(
            "comment_editor_channel_note",
            render_comment_channel_snapshot(Channel::Note)
        );
    }

    #[test]
    fn tui_snapshot_comment_editor_live_channel_cycle() {
        let session = snapshot_session("");
        let mut editor = CommentEditor::with_channel("unchanged text".into(), Channel::Onboarding);
        editor.cycle_channel();
        let mode = Mode::CommentInput {
            editor,
            target: CommentInputTarget::NewGeneral,
        };

        insta::assert_snapshot!(
            "comment_editor_live_channel_cycle",
            format!(
                "cycled onboarding → delegation\n{}",
                render_tui_text(&session, &mode, 90, 18)
            )
        );
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

    fn session_with_all_channel_comments() -> ReviewSession {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,4 @@\n-old\n+onboarding\n+delegation\n+collaboration\n+note\n",
        );
        session.toggle_focus();
        for (line, channel, id) in [
            (1, Channel::Onboarding, "onboard"),
            (2, Channel::Delegation, "delegate"),
            (3, Channel::Collaboration, "collab"),
            (4, Channel::Note, "private"),
        ] {
            let row = session
                .diff_rows_for_selected_file()
                .iter()
                .position(|row| row.new_lineno == Some(line))
                .unwrap();
            session.select_diff_row(row);
            session.add_comment_in_channel(format!("{id} comment"), channel);
            session.comments.last_mut().unwrap().id = id.into();
        }
        session
    }

    #[test]
    fn tui_snapshot_channel_colored_gutter_marks() {
        let session = session_with_all_channel_comments();
        let theme = AppTheme::default();
        let rows = session.diff_rows_for_selected_file();
        let colors = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let count = row_comment_count(&session, row);
                (count > 0).then(|| {
                    let line = unified_row_line(&session, row, index, count, &theme);
                    format!(
                        "{}={:?}",
                        row.text,
                        line.spans.first().and_then(|span| span.style.fg)
                    )
                })
            })
            .collect::<Vec<_>>()
            .join(" · ");

        insta::assert_snapshot!(
            "annotation_channel_gutter_rows",
            format!(
                "gutter styles: {colors}\n{}",
                render_tui_text(&session, &Mode::Normal, 100, 36)
            )
        );
    }

    #[test]
    fn mixed_channel_gutter_mark_prefers_most_actionable_channel() {
        let raw = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n";
        let commented_channel = |session: &ReviewSession| {
            session
                .diff_rows_for_selected_file()
                .iter()
                .find_map(|row| row_comment_channel(session, row))
        };

        // Ascending actionability: each more actionable comment takes over.
        let mut session = snapshot_session(raw);
        session.toggle_focus();
        let row = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.new_lineno == Some(1))
            .unwrap();
        session.select_diff_row(row);
        for channel in [
            Channel::Note,
            Channel::Onboarding,
            Channel::Collaboration,
            Channel::Delegation,
        ] {
            session.add_comment_in_channel(format!("{channel:?}"), channel);
            assert_eq!(commented_channel(&session), Some(channel));
        }

        // Descending insertion order resolves identically: deliberate
        // delegation > collaboration > onboarding > note priority decides the
        // mark, never whichever comment happens to be first on the anchor.
        let mut session = snapshot_session(raw);
        session.toggle_focus();
        session.select_diff_row(row);
        for channel in [
            Channel::Delegation,
            Channel::Collaboration,
            Channel::Onboarding,
            Channel::Note,
        ] {
            session.add_comment_in_channel(format!("{channel:?}"), channel);
            assert_eq!(commented_channel(&session), Some(Channel::Delegation));
        }
    }

    #[test]
    fn tui_snapshot_channel_colored_inline_card_style_runs() {
        let session = session_with_all_channel_comments();
        let mode = Mode::Normal;
        let (buffer, _) = render_tui_buffer_and_cursor(&session, &mode, 100, 36);

        insta::assert_snapshot!(
            "annotation_channel_inline_card_style_runs",
            style_runs_for_rows(
                &buffer,
                &[
                    "onboard comment",
                    "delegate comment",
                    "collab comment",
                    "private comment",
                ],
            )
        );
    }

    #[test]
    fn tui_snapshot_all_channel_comment_list_rows() {
        let session = session_with_all_channel_comments();
        let mode = Mode::CommentList(CommentListState { selected: 2 });
        let (buffer, _) = render_tui_buffer_and_cursor(&session, &mode, 110, 24);

        insta::assert_snapshot!(
            "annotation_channel_comment_list_rows",
            format!(
                "{}\nstyle runs:\n{}",
                render_tui_text(&session, &mode, 110, 24),
                style_runs_for_rows(
                    &buffer,
                    &[
                        "onboard comment",
                        "delegate comment",
                        "collab comment",
                        "private comment",
                    ],
                )
            )
        );
    }

    #[test]
    fn tui_snapshot_production_agent_draft_rows() {
        let mut session = session_with_all_channel_comments();
        session.agent_identity = Identity {
            kind: AuthorKind::Agent,
            name: "configured-review-agent".into(),
        };
        let accepted = session
            .add_agent_draft(
                "a.txt".into(),
                Some(1),
                "accepted collaboration is filtered".into(),
            )
            .unwrap();
        let accepted_id = session
            .accept_agent_draft(&accepted, accepted.body.clone(), Channel::Collaboration)
            .unwrap();
        let accepted = session
            .comments
            .iter()
            .find(|comment| comment.id == accepted_id)
            .unwrap();
        assert_eq!(accepted.author, session.agent_identity);
        assert_eq!(accepted.channel, Channel::Collaboration);
        assert_eq!(accepted.state, CommentState::Todo);
        session
            .add_agent_draft(
                "a.txt".into(),
                Some(1),
                "agent onboarding line draft".into(),
            )
            .unwrap();
        session
            .add_agent_draft("a.txt".into(), None, "agent onboarding file draft".into())
            .unwrap();
        let mut drafts = DraftListState::new(&session);
        assert_eq!(drafts.drafts.len(), 2);
        assert!(drafts.drafts.iter().all(|draft| {
            draft.author == session.agent_identity
                && draft.channel == Channel::Onboarding
                && draft.state == CommentState::Draft
        }));
        assert!(drafts.drafts.iter().all(|draft| {
            !matches!(
                draft.body.as_str(),
                "onboard comment" | "delegate comment" | "collab comment" | "private comment"
            )
        }));
        drafts.move_selection(1);
        let mode = Mode::DraftList(drafts);
        let (buffer, _) = render_tui_buffer_and_cursor(&session, &mode, 110, 24);

        insta::assert_snapshot!(
            "annotation_channel_draft_rows",
            format!(
                "{}\nstyle runs:\n{}",
                render_tui_text(&session, &mode, 110, 24),
                style_runs_for_rows(
                    &buffer,
                    &["agent onboarding line draft", "agent onboarding file draft",],
                )
            )
        );
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
        session.comments[1].channel = Channel::Delegation;
        session.comments[2].id = "standalone-feedback".to_owned();
        session.comments[2].state = crate::state::CommentState::Todo;
        session.comments[2].channel = Channel::Collaboration;
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
        let (buffer, _) = render_tui_buffer_and_cursor(&session, &mode, 100, 24);

        insta::assert_snapshot!(format!(
            "{}\nstyle runs:\n{}",
            render_tui_text(&session, &mode, 100, 24),
            style_runs_for_rows(&buffer, &["[evidence] [→ agent]", "[feedback] [→ team]"],)
        ));
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

        let unscrolled = render_tui_text(&session, &Mode::Normal, 100, 16);
        assert!(unscrolled.contains("note on new"));

        // Scrolling past the commented row must not shift or duplicate the
        // remaining lines: line 4 of the full render becomes the first
        // diff line after scrolling by 4.
        session.diff_scroll = 4;
        let scrolled = render_tui_text(&session, &Mode::Normal, 100, 16);
        assert!(scrolled.contains("note on new"));
        assert_eq!(scrolled.matches("note on new").count(), 1);
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
    fn measured_content_width_tracks_excerpt_but_stays_readable() {
        assert_eq!(measured_content_width(200, 48), 72);
        assert_eq!(measured_content_width(200, 88), 88);
        assert_eq!(measured_content_width(200, 140), 100);
        assert_eq!(measured_content_width(80, 140), 80);
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
        session
            .add_agent_draft("a.txt".into(), Some(1), "consider a clearer name".into())
            .unwrap();
        session.comments.last_mut().unwrap().id = "draft-1".into();
        session
            .add_agent_draft("a.txt".into(), None, "file-level: needs tests".into())
            .unwrap();
        session.comments.last_mut().unwrap().id = "draft-2".into();
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
    fn tui_snapshot_theme_popup_clearing_transparent_and_opaque() {
        let session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,2 +1,2 @@
-    old();
+    new();
"#,
        );

        // Default theme: dark, truecolor, transparent — popup clears must
        // leave the terminal background visible (bg Reset) outside diff and
        // selection surfaces while still forcing the themed foreground.
        let transparent = TuiState::default();
        let transparent_runs =
            render_tui_style_runs_with_state(&session, &Mode::Help, &transparent, 60, 16);
        assert!(
            transparent_runs.contains("bg=Some(Reset)"),
            "{transparent_runs}"
        );
        insta::assert_snapshot!(
            "tui_snapshot_theme_popup_clearing_transparent",
            transparent_runs
        );

        // Opaque dark theme: the base fill and popup clears paint the
        // theme's background behind every popup cell.
        let opaque_theme =
            AppTheme::resolve(crate::config::ThemeModeConfig::Dark, false, true, None);
        let opaque = TuiState {
            theme: opaque_theme,
            ..TuiState::default()
        };
        let opaque_runs = render_tui_style_runs_with_state(&session, &Mode::Help, &opaque, 60, 16);
        let expected_bg = format!("bg=Some({:?})", opaque_theme.background);
        assert!(opaque_runs.contains(&expected_bg), "{opaque_runs}");
        assert!(!opaque_runs.contains("bg=Some(Reset)"), "{opaque_runs}");
        insta::assert_snapshot!("tui_snapshot_theme_popup_clearing_opaque", opaque_runs);
    }

    #[test]
    fn rendered_diff_output_meets_the_documented_contrast_contract() {
        // Regression for the review finding that surfaces were guarded only
        // against the primary foreground while positive/negative text
        // actually rendered on them. Walk every *rendered* cell — emphasis,
        // line backgrounds, cursor row, active range, footer — and assert
        // each fg/bg pair meets its documented target in the final output
        // space (docs/theme.md "Contrast contract"): 4.5:1 in general,
        // 3.0:1 for muted text and for colored semantic text on transient
        // highlight surfaces.
        use super::super::theme::{contrast_ratio, final_rgb};

        let plain_session = snapshot_session(
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
        let mut cursor_session = plain_session.clone();
        cursor_session.toggle_focus();
        let mut range_session = cursor_session.clone();
        range_session.set_diff_range_selection(3, 4);

        for truecolor in [true, false] {
            for transparent in [true, false] {
                let theme = AppTheme::resolve(
                    crate::config::ThemeModeConfig::Dark,
                    transparent,
                    truecolor,
                    None,
                );
                let tui_state = TuiState {
                    theme,
                    ..TuiState::default()
                };
                let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
                let expected_added_word_bg = theme.added_word.bg.expect("added word bg");
                let expected_removed_word_bg = theme.removed_word.bg.expect("removed word bg");
                let mut saw_added_line = false;
                let mut saw_removed_line = false;
                let mut saw_added_word = false;
                let mut saw_removed_word = false;
                let mut saw_selection = false;
                let mut saw_range = false;

                for (scenario, session) in [
                    ("plain", &plain_session),
                    ("cursor", &cursor_session),
                    ("range", &range_session),
                ] {
                    let backend = TestBackend::new(80, 14);
                    let mut terminal = Terminal::new(backend).unwrap();
                    terminal
                        .draw(|frame| {
                            draw(frame, session, &Mode::Normal, &keymap, &tui_state, None)
                        })
                        .unwrap();
                    let buffer = terminal.backend().buffer();
                    for y in 0..buffer.area.height {
                        for x in 0..buffer.area.width {
                            let cell = &buffer[(x, y)];
                            if cell.symbol().trim().is_empty() {
                                continue;
                            }
                            let style = cell.style();
                            if style.add_modifier.contains(Modifier::DIM) {
                                continue;
                            }
                            saw_added_line |= style.bg == Some(theme.added_line_bg);
                            saw_removed_line |= style.bg == Some(theme.removed_line_bg);
                            saw_added_word |= style.bg == Some(expected_added_word_bg)
                                && style.fg == Some(theme.foreground);
                            saw_removed_word |= style.bg == Some(expected_removed_word_bg)
                                && style.fg == Some(theme.foreground);
                            saw_selection |= style.bg == Some(theme.selection_bg);
                            saw_range |= style.bg == Some(theme.range_bg);

                            let Some(fg) = style.fg.and_then(final_rgb) else {
                                continue;
                            };
                            let bg = style
                                .bg
                                .and_then(final_rgb)
                                .unwrap_or(theme.contrast_background);
                            let highlight = style.bg == Some(theme.selection_bg)
                                || style.bg == Some(theme.range_bg);
                            let plain_text = style.fg == Some(theme.foreground)
                                || style.fg == Some(theme.subtle);
                            let min = if style.fg == Some(theme.muted) || (highlight && !plain_text)
                            {
                                3.0
                            } else {
                                4.5
                            };
                            let ratio = contrast_ratio(fg, bg);
                            assert!(
                                ratio >= min,
                                "scenario={scenario} truecolor={truecolor} transparent={transparent} \
                                 cell ({x},{y}) {:?} fg={:?} bg={:?} ratio={ratio:.2} < {min}",
                                cell.symbol(),
                                style.fg,
                                style.bg,
                            );
                        }
                    }
                }
                assert!(saw_added_line, "missing added line surface");
                assert!(saw_removed_line, "missing removed line surface");
                assert!(saw_added_word, "missing added changed-word tint");
                assert!(saw_removed_word, "missing removed changed-word tint");
                assert!(saw_selection, "missing cursor selection surface");
                assert!(saw_range, "missing range surface");
            }
        }
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
        use crate::tui::theme::{Rgb, nearest_indexed};
        // Exact cube colors map to their cube index.
        assert_eq!(nearest_indexed(Rgb::new(0, 0, 0)), 16);
        assert_eq!(nearest_indexed(Rgb::new(255, 255, 255)), 231);
        assert_eq!(nearest_indexed(Rgb::new(0, 95, 0)), 22);
        // Near-grays prefer the grayscale ramp over the coarse cube.
        assert_eq!(nearest_indexed(Rgb::new(0x12, 0x12, 0x12)), 233);
        // Dark tints keep their hue instead of flattening to black/gray.
        assert_eq!(nearest_indexed(Rgb::new(0x12, 0x26, 0x1e)), 22);
        assert_eq!(nearest_indexed(Rgb::new(0x30, 0x1b, 0x1f)), 52);

        assert_eq!(quantize_spec("bold on #1a4a29"), "bold on 22");
        assert_eq!(quantize_spec("#3fb950"), "71");
        // Named and indexed tokens pass through untouched.
        assert_eq!(quantize_spec("green bold"), "green bold");
        assert_eq!(quantize_spec("28"), "28");
    }

    #[test]
    fn downgrade_diff_theme_rewrites_all_hex_entries() {
        let mut theme = crate::config::DiffThemeConfig {
            added_line_bg: Some("#12261e".to_owned()),
            removed_line_bg: Some("#301b1f".to_owned()),
            added_word: Some("bold on #1a4a29".to_owned()),
            removed_word: Some("bold on #6b2b2b".to_owned()),
            gutter_added: Some("#3fb950".to_owned()),
            gutter_removed: Some("#f85149".to_owned()),
        };

        downgrade_diff_theme(&mut theme);

        for spec in [
            &theme.added_line_bg,
            &theme.removed_line_bg,
            &theme.added_word,
            &theme.removed_word,
            &theme.gutter_added,
            &theme.gutter_removed,
        ] {
            let spec = spec.as_deref().expect("explicit specs survive downgrade");
            assert!(!spec.contains('#'), "hex survived downgrade: {spec}");
            assert!(
                syntax_style_spec(spec) != Style::default(),
                "spec parses: {spec}"
            );
        }

        // Unset (derived) entries stay unset: quantization happens inside
        // AppTheme::resolve for those, never here.
        let mut derived = crate::config::DiffThemeConfig::default();
        downgrade_diff_theme(&mut derived);
        assert_eq!(derived, crate::config::DiffThemeConfig::default());
        assert_eq!(derived.added_line_bg, None);
        assert_eq!(derived.gutter_removed, None);
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
        session.toggle_focus();
        session.file_pane_visible = false;

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
        session.toggle_focus();
        session.file_pane_visible = false;

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
                materialize_diff_source(&session, &rows, layout.line_number_width, 0, &line.source)
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
                .is_some_and(|span| span.style.bg == Some(AppTheme::default().selection_bg))
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
                        0,
                        &line.source,
                    )
                    .spans,
                )
            })
            .unwrap();

        let tui_state = TuiState::default();
        scroll_diff_horizontal_visual(&mut session, inner, 10, &tui_state);
        let after_layout = measured_diff_layout(&session, &rows, inner, false);
        let horizontal = tui_state.diff_viewport.visual_state(&session).1;
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
                        horizontal,
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
            .saturating_sub(split.viewport_start(
                session.diff_scroll as usize,
                0,
                inner.height as usize,
            ));
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
            0,
            &before_line.source,
        );
        let before_text = spans_text(&before_painted.spans);

        let tui_state = TuiState::default();
        scroll_diff_horizontal_visual(&mut session, inner, 10, &tui_state);
        let after_layout = measured_diff_layout(&session, &rows, inner, true);
        let horizontal = tui_state.diff_viewport.visual_state(&session).1;
        let after_line = after_layout
            .lines
            .iter()
            .find(|line| line.hit.contains(left))
            .unwrap();
        let after_painted = materialize_diff_source(
            &session,
            &rows,
            after_layout.line_number_width,
            horizontal,
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
        let rows = session.diff_rows_for_selected_file();
        let wide = measured_diff_layout(&session, &rows, Rect::new(0, 0, 90, 10), false);
        let start = wide.viewport_start(session.diff_scroll as usize, 100, 10);

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
        let inner = Rect::new(0, 0, 28, 3);
        let tui_state = TuiState::default();
        tui_state
            .diff_viewport
            .visual_scroll(&mut session, inner, 5);

        ensure_diff_cursor_visible(&mut session, inner, &tui_state);

        assert_eq!(session.diff_scroll as usize, owner);
        assert_eq!(tui_state.diff_viewport.visual_state(&session).0, 0);
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

        assert_eq!(
            layout.viewport_start(session.diff_scroll as usize, 0, 2),
            expected
        );
    }

    #[test]
    fn stale_horizontal_offset_is_clamped_after_reflow_or_refresh() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+short\n",
        );
        session.diff_cues.soft_wrap = false;
        let rows = session.diff_rows_for_selected_file();
        let owner = rows.iter().position(|row| row.text == "short").unwrap();
        let inner = Rect::new(0, 0, 80, 10);

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
                        0,
                        &line.source,
                    )
                    .spans,
                )
            })
            .unwrap();
        assert!(rendered.contains("short"));

        let tui_state = TuiState::default();
        scroll_diff_horizontal_visual(&mut session, inner, 500, &tui_state);
        assert_eq!(tui_state.diff_viewport.visual_state(&session).1, 0);
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
        let inner = Rect::new(0, 0, 40, 5);
        let tui_state = TuiState::default();
        tui_state
            .diff_viewport
            .visual_scroll(&mut session, inner, isize::MAX);
        let rows = session.diff_rows_for_selected_file();
        let first = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);

        reconcile_diff_viewport(&mut session, inner, false, &tui_state);
        let second = cached_diff_layout(&session, rows, inner, false, &tui_state);

        assert!(Rc::ptr_eq(&first, &second));
        assert_eq!(tui_state.diff_viewport.cache_builds(), 1);
        assert!(tui_state.diff_viewport.visual_state(&session).0 < first.lines.len());
        let window = materialize_diff_window(
            &session,
            &session.diff_rows_for_selected_file(),
            &first,
            0,
            5,
            0,
            None,
            &AppTheme::default(),
        );
        assert_eq!(window.len(), 5);
    }

    fn push_row_comment(session: &mut ReviewSession, row: usize, id: &str, body: &str) {
        let anchor = session.diff_rows_for_selected_file()[row]
            .anchor
            .clone()
            .expect("comment row must be anchorable");
        session.comments.push(Comment {
            id: id.to_owned(),
            path: Some(anchor.path().to_owned()),
            line: anchor.line(),
            end_line: None,
            anchor: Some(anchor),
            body: body.to_owned(),
            ..Default::default()
        });
    }

    #[test]
    fn geometry_cache_invalidates_for_all_selected_file_annotation_inputs() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let inner = Rect::new(0, 0, 40, 5);
        let tui_state = TuiState::default();
        let rows = session.diff_rows_for_selected_file();
        let old = rows.iter().position(|row| row.text == "old").unwrap();
        let new = rows.iter().position(|row| row.text == "new").unwrap();
        let mut previous = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);

        push_row_comment(&mut session, old, "selected-comment", "first summary");
        let added = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);
        assert!(!Rc::ptr_eq(&previous, &added));
        previous = added;

        session.comments[0].body = "changed summary".to_owned();
        let summary = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);
        assert!(!Rc::ptr_eq(&previous, &summary));
        previous = summary;

        session.comments[0].state = CommentState::Todo;
        session.comments[0].channel = Channel::Delegation;
        let state = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);
        assert!(!Rc::ptr_eq(&previous, &state));
        previous = state;

        session.comments[0].action = Some(ActionIntent::Fix);
        let action = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);
        assert!(!Rc::ptr_eq(&previous, &action));
        previous = action;

        session.comments[0].kind = Some(CommentKind::Issue);
        let kind = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);
        assert!(!Rc::ptr_eq(&previous, &kind));
        previous = kind;

        session.comments[0].replies.push(CommentReply {
            body: "visible reply".to_owned(),
            ..Default::default()
        });
        let reply = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);
        assert!(!Rc::ptr_eq(&previous, &reply));
        previous = reply;

        let new_anchor = rows[new].anchor.clone().unwrap();
        session.comments[0].path = Some(new_anchor.path().to_owned());
        session.comments[0].line = new_anchor.line();
        session.comments[0].end_line = None;
        session.comments[0].anchor = Some(new_anchor);
        let ownership = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);
        assert!(!Rc::ptr_eq(&previous, &ownership));
        previous = ownership;

        session.comments.clear();
        let removed = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);
        assert!(!Rc::ptr_eq(&previous, &removed));
        previous = removed;

        session.files[0].changed_hunks.insert(0);
        let changed_hunk = cached_diff_layout(&session, rows, inner, false, &tui_state);
        assert!(!Rc::ptr_eq(&previous, &changed_hunk));
        assert_eq!(tui_state.diff_viewport.cache_builds(), 10);
    }

    #[test]
    fn geometry_cache_reuses_for_unrelated_detail_and_style_mutations() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old a\n+new a\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old b\n+new b\n",
        );
        let inner = Rect::new(0, 0, 50, 8);
        let tui_state = TuiState::default();
        let rows = session.diff_rows_for_selected_file();
        let selected = rows.iter().position(|row| row.text == "old a").unwrap();
        push_row_comment(
            &mut session,
            selected,
            "selected-comment",
            "selected summary\noriginal detail",
        );
        let first = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);

        let unrelated_anchor =
            crate::anchor::comment_anchor_for_file_lines(&session.files[1], Some(1), None).unwrap();
        session.comments.insert(
            0,
            Comment {
                id: "unrelated-comment".to_owned(),
                path: Some(unrelated_anchor.path().to_owned()),
                line: unrelated_anchor.line(),
                anchor: Some(unrelated_anchor),
                body: "unrelated summary".to_owned(),
                ..Default::default()
            },
        );
        let unrelated_insert = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);
        assert!(Rc::ptr_eq(&first, &unrelated_insert));

        let rendered_comment = first
            .lines
            .iter()
            .filter_map(|line| match &line.source {
                DiffVisualSource::Annotation { .. } => Some(spans_text(
                    &materialize_diff_source(
                        &session,
                        &rows,
                        first.line_number_width,
                        0,
                        &line.source,
                    )
                    .spans,
                )),
                _ => None,
            })
            .collect::<String>();
        assert!(rendered_comment.contains("selected summary"));
        assert!(!rendered_comment.contains("unrelated summary"));

        session.comments[0].body = "mutated unrelated summary".to_owned();
        let unrelated_edit = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);
        assert!(Rc::ptr_eq(&first, &unrelated_edit));

        session.comments.push(Comment {
            id: "general-comment".to_owned(),
            body: "general summary".to_owned(),
            ..Default::default()
        });
        let general = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);
        assert!(Rc::ptr_eq(&first, &general));

        let file_anchor =
            crate::anchor::comment_anchor_for_file_lines(&session.files[0], None, None).unwrap();
        session.comments.push(Comment {
            id: "file-comment".to_owned(),
            path: Some(file_anchor.path().to_owned()),
            anchor: Some(file_anchor),
            body: "file-only summary".to_owned(),
            ..Default::default()
        });
        session.comments[2].body = "mutated general summary".to_owned();
        session.comments[3].body = "mutated file-only summary".to_owned();
        session.comments[1].updated_at = Some(chrono::Utc::now());
        session.move_diff_cursor(1);
        session.toggle_focus();
        session.set_diff_range_selection(selected, selected);
        session.syntax.theme.keyword = "red bold".to_owned();
        let detail_and_style = cached_diff_layout(&session, rows, inner, false, &tui_state);
        assert!(Rc::ptr_eq(&first, &detail_and_style));
        assert_eq!(tui_state.diff_viewport.cache_builds(), 1);
    }

    #[test]
    fn cache_hit_uses_signature_without_reprojecting_payloads() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        );
        session.toggle_focus();
        session.add_comment("headline\nlarge body that should not clone again".repeat(100));
        session.comments[0].replies.push(CommentReply {
            body: "reply payload".repeat(100),
            ..Default::default()
        });
        let rows = session.diff_rows_for_selected_file();
        let state = TuiState::default();
        super::super::annotation_card::reset_projection_count();
        let first = cached_diff_layout(
            &session,
            rows.clone(),
            Rect::new(0, 0, 72, 20),
            false,
            &state,
        );
        let projections = super::super::annotation_card::projection_count();
        assert!(projections > 0);
        let second = cached_diff_layout(&session, rows, Rect::new(0, 0, 72, 20), false, &state);
        assert!(Rc::ptr_eq(&first, &second));
        assert_eq!(
            super::super::annotation_card::projection_count(),
            projections
        );
    }

    #[test]
    fn geometry_cache_invalidates_for_view_and_rows_inputs() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new long enough to wrap\n",
        );
        let tui_state = TuiState::default();
        let rows = session.diff_rows_for_selected_file();
        let first = cached_diff_layout(
            &session,
            rows.clone(),
            Rect::new(0, 0, 40, 5),
            false,
            &tui_state,
        );
        let width = cached_diff_layout(
            &session,
            rows.clone(),
            Rect::new(0, 0, 30, 5),
            false,
            &tui_state,
        );
        assert!(!Rc::ptr_eq(&first, &width));

        let split = cached_diff_layout(
            &session,
            rows.clone(),
            Rect::new(0, 0, 30, 5),
            true,
            &tui_state,
        );
        assert!(!Rc::ptr_eq(&width, &split));

        session.diff_cues.soft_wrap = !session.diff_cues.soft_wrap;
        let wrap = cached_diff_layout(
            &session,
            rows.clone(),
            Rect::new(0, 0, 30, 5),
            true,
            &tui_state,
        );
        assert!(!Rc::ptr_eq(&split, &wrap));

        let replacement_rows = Rc::new(rows.as_ref().clone());
        let replaced = cached_diff_layout(
            &session,
            replacement_rows,
            Rect::new(0, 0, 30, 5),
            true,
            &tui_state,
        );
        assert!(!Rc::ptr_eq(&wrap, &replaced));
        assert_eq!(tui_state.diff_viewport.cache_builds(), 5);
    }

    #[test]
    fn range_comment_annotation_owners_are_deterministic() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let rows = session.diff_rows_for_selected_file();
        let old = rows.iter().position(|row| row.text == "old").unwrap();
        let new = rows.iter().position(|row| row.text == "new").unwrap();
        session.set_diff_range_selection(old, new);
        let anchor = session.selected_range_anchor().unwrap();
        session.comments.push(Comment {
            id: "range-comment".to_owned(),
            path: Some(anchor.path().to_owned()),
            line: anchor.line(),
            end_line: anchor.end_line(),
            anchor: Some(anchor),
            body: "range summary".to_owned(),
            ..Default::default()
        });

        let input = selected_file_annotations(&session, &rows).input;
        let owners: Vec<_> = input.cards.iter().map(|card| card.owner).collect();
        assert_eq!(owners, [new]);
    }

    #[test]
    fn right_side_range_owner_is_shared_by_navigation_unified_and_split() {
        let mut session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1,2 @@\n-old\n+new one\n+new two\n",
        );
        session.toggle_focus();
        let rows = session.diff_rows_for_selected_file();
        let first = rows.iter().position(|row| row.text == "new one").unwrap();
        let second = rows.iter().position(|row| row.text == "new two").unwrap();
        session.set_diff_range_selection(first, second);
        session.add_comment("right-side range\nsecond card line in side-by-side view".into());
        session.comments[0].id = "range".into();
        assert_eq!(
            session.selected_comment_card_owner(&session.comments[0]),
            Some(second)
        );

        session.select_comment_by_id("range");
        assert_eq!(session.diff_cursor, second);
        let rows = session.diff_rows_for_selected_file();
        for split in [false, true] {
            let layout = measured_diff_layout(
                &session,
                &rows,
                Rect::new(0, 0, if split { 120 } else { 72 }, 30),
                split,
            );
            let cards = layout
                .lines
                .iter()
                .filter(|line| line.is_comment)
                .collect::<Vec<_>>();
            assert!(!cards.is_empty());
            assert!(cards.iter().all(|line| line.block_anchor == second));
            assert!(cards.iter().all(|line| line.hit.row_at(0) == Some(second)));
        }
        session.file_pane_visible = false;
        session.toggle_diff_view();
        insta::assert_snapshot!(
            "inline_annotation_card_side_by_side_range",
            render_tui_text(&session, &Mode::Normal, 130, 24)
        );
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
                line.hit.contains(owner)
                    && matches!(line.source, DiffVisualSource::Annotation { .. })
            })
            .collect();
        assert!(!comment_lines.is_empty());

        session.diff_scroll = owner as u16;
        let tui_state = TuiState::default();
        scroll_diff_visual(&mut session, inner, 2, &tui_state);
        assert_eq!(session.diff_scroll as usize, owner);
        assert!(tui_state.diff_viewport.visual_state(&session).0 > 0);
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
        let continuation = tui_state.diff_viewport.visual_state(&session).0;
        let start = layout.viewport_start(session.diff_scroll as usize, continuation, 6);

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
        session.file_pane_visible = false;
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

        let style = syntax_span_style(&span, false, false, &theme, &AppTheme::default());

        assert_eq!(style.fg, Some(Color::Red));
        assert!(style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn annotation_card_text_matches_rendered_lines() {
        // Measurement consumes the card's plain lines while drawing renders
        // the same semantic segments with a theme. They must remain textually
        // identical or viewport geometry and hit ownership drift.
        let comment = Comment {
            id: "abcdef1234567890".to_owned(),
            body: "\n  headline text  \nrest of the body".to_owned(),
            action: Some(ActionIntent::Fix),
            kind: Some(CommentKind::Question),
            state: CommentState::Todo,
            channel: Channel::Delegation,
            ..Default::default()
        };
        let card = AnnotationCard::from_comment(&comment);
        let layout = card.layout(54, AnnotationCardDensity::Expanded, false, "E");
        for index in 0..layout.len() {
            assert_eq!(
                layout.plain_line(index),
                spans_text(
                    &layout
                        .line(index, card.channel, false, false, &AppTheme::default())
                        .spans
                )
            );
        }
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
        assert!(rendered.contains("human:local  [→ note]  [draft]"));
        assert!(rendered.contains("first note"));
        assert!(rendered.contains("second note"));
    }

    #[test]
    fn colocated_card_hits_keep_exact_comment_identity() {
        let mut session = snapshot_session(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        );
        session.toggle_focus();
        session.add_comment("first".into());
        session.add_comment("second".into());
        session.comments[0].id = "first".into();
        session.comments[1].id = "second".into();
        let rows = session.diff_rows_for_selected_file();
        let layout = measured_diff_layout(&session, &rows, Rect::new(0, 0, 72, 30), false);
        let hits = layout
            .lines
            .iter()
            .enumerate()
            .filter_map(|(line, visual)| match &visual.hit {
                DiffVisualHit::Annotation { source, .. } => Some((line, source.clone())),
                _ => None,
            })
            .fold(
                Vec::<(usize, AnnotationSource)>::new(),
                |mut found, value| {
                    if !found.iter().any(|(_, source)| source == &value.1) {
                        found.push(value);
                    }
                    found
                },
            );
        assert_eq!(hits.len(), 2);
        for (line, source) in hits {
            assert_eq!(
                layout.hit_at(line, 20),
                Some(DiffPointHit::Annotation {
                    owner: session.diff_cursor,
                    source,
                })
            );
        }
        session.file_pane_visible = false;
        let state = TuiState::default();
        state.diff_viewport.select_annotation(
            &session,
            AnnotationSource::Comment {
                id: "second".into(),
            },
        );
        let buffer = render_tui_buffer_with_state(&session, &Mode::Normal, &state, 84, 24);
        insta::assert_snapshot!(
            "inline_annotation_colocated_exact_selection",
            format!(
                "{}\nstyle runs:\n{}",
                render_tui_text_with_state(&session, &Mode::Normal, &state, 84, 24),
                style_runs_for_rows(&buffer, &["first", "second"])
            )
        );
    }

    #[test]
    fn tui_snapshot_selected_and_range_inline_annotation_cards() {
        let mut session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n-old one\n-old two\n+new one\n+new two\n",
        );
        session.file_pane_visible = false;
        session.toggle_focus();
        let rows = session.diff_rows_for_selected_file();
        let first = rows.iter().position(|row| row.text == "new one").unwrap();
        let second = rows.iter().position(|row| row.text == "new two").unwrap();
        session.select_diff_row(first);
        session
            .add_comment("Selected agent draft\nThe detailed explanation remains inline.".into());
        session.comments[0].author = Identity {
            kind: AuthorKind::Agent,
            name: "review-agent".into(),
        };
        session.comments[0].channel = Channel::Onboarding;
        session.set_diff_range_selection(first, second);
        session.add_comment("Range todo\nOwn the card at the range endpoint.".into());
        session.comments[1].state = CommentState::Todo;
        session.comments[1].channel = Channel::Delegation;
        session.comments[1].kind = Some(CommentKind::Issue);
        session.comments[1].action = Some(ActionIntent::Fix);
        session.set_diff_range_selection(first, second);

        insta::assert_snapshot!(
            "inline_annotation_cards_selected_and_range",
            render_tui_text(&session, &Mode::Normal, 88, 28)
        );
    }

    #[test]
    fn spotlight_card_artifacts_expand_ephemerally_and_keep_hit_ownership() {
        let mut session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new parser entry\n",
        );
        session.file_pane_visible = false;
        session.toggle_focus();
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let target = crate::attention::target_for_diff(&files, "a.rs", Some(1), None).unwrap();
        let step = WalkthroughStep {
            id: "spotlight".into(),
            author: Some(Identity::agent()),
            target: target.clone(),
            title: Some("Start at the parser boundary".into()),
            why: Some("It controls every downstream error.".into()),
            body: Some("The parser validates before committing state.".into()),
            artifacts: vec![
                StepArtifact {
                    title: "usage".into(),
                    kind: StepArtifactKind::Example,
                    body: "parse(input)?.commit()".into(),
                },
                StepArtifact {
                    title: "flow".into(),
                    kind: StepArtifactKind::Diagram,
                    body: "input -> validate -> commit".into(),
                },
            ],
            ..Default::default()
        };
        let durable = super::super::ensure_tui_review_session(&mut session);
        durable.walkthroughs.push(Walkthrough {
            id: "walkthrough".into(),
            steps: vec![step],
            ..Default::default()
        });
        durable.attention_regions.push(AttentionRegion {
            target,
            salience: Salience::Spotlight,
            rationale: Some("effective rationale".into()),
            source: SalienceSource::Agent,
        });
        let owner = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "new parser entry")
            .unwrap();
        session.select_diff_row(owner);
        let tui_state = TuiState::default();
        let rows = session.diff_rows_for_selected_file();
        let inner = Rect::new(0, 0, 72, 30);
        let collapsed = cached_diff_layout(&session, rows.clone(), inner, false, &tui_state);
        assert!(
            collapsed
                .lines
                .iter()
                .filter(|line| line.is_comment)
                .all(|line| {
                    line.hit.row_at(0) == Some(owner) && line.hit.row_at(71) == Some(owner)
                })
        );
        assert_eq!(
            tui_state
                .diff_viewport
                .toggle_annotation_artifacts(&session),
            Some(true)
        );
        let expanded = cached_diff_layout(&session, rows, inner, false, &tui_state);
        assert!(expanded.line_count() > collapsed.line_count());
        assert!(
            expanded
                .lines
                .iter()
                .filter(|line| line.is_comment)
                .all(|line| {
                    line.hit.row_at(0) == Some(owner) && line.hit.row_at(71) == Some(owner)
                })
        );

        insta::assert_snapshot!(
            "inline_spotlight_card_expanded_artifacts",
            render_tui_text_with_state(&session, &Mode::Normal, &tui_state, 76, 30)
        );
    }

    #[test]
    fn colocated_artifact_cards_expand_independently_and_use_live_hint() {
        let mut session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        session.file_pane_visible = false;
        session.toggle_focus();
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let target = crate::attention::target_for_diff(&files, "a.rs", Some(1), None).unwrap();
        let steps = [
            ("first", "first artifact", "FIRST BODY"),
            ("second", "second artifact", "SECOND BODY"),
        ]
        .into_iter()
        .map(|(id, artifact_title, artifact_body)| WalkthroughStep {
            id: id.into(),
            author: Some(Identity::agent()),
            target: target.clone(),
            title: Some(format!("{id} narration")),
            artifacts: vec![StepArtifact {
                title: artifact_title.into(),
                kind: StepArtifactKind::Example,
                body: artifact_body.into(),
            }],
            ..Default::default()
        })
        .collect::<Vec<_>>();
        let durable = super::super::ensure_tui_review_session(&mut session);
        durable.walkthroughs.push(Walkthrough {
            id: "walk".into(),
            steps,
            ..Default::default()
        });
        durable.attention_regions.push(AttentionRegion {
            target,
            salience: Salience::Spotlight,
            rationale: None,
            source: SalienceSource::Agent,
        });
        let owner = session
            .selected_walkthrough_card_owner(&session.sessions[0].walkthroughs[0].steps[0].target)
            .unwrap();
        session.select_diff_row(owner);
        let state = TuiState::default();
        let second = AnnotationSource::Walkthrough {
            step_id: "second".into(),
            part: 0,
        };
        state.diff_viewport.select_annotation(&session, second);
        assert_eq!(
            state.diff_viewport.toggle_annotation_artifacts(&session),
            Some(true)
        );

        let config = KeybindingsConfig {
            toggle_annotation_artifacts: vec!["alt-e".into()],
            ..KeybindingsConfig::default()
        };
        let keymap = KeyMap::try_from(&config).unwrap();
        let second_only =
            render_tui_text_with_state_and_keymap(&session, &Mode::Normal, &state, &keymap, 90, 34);
        assert!(second_only.contains("SECOND BODY"));
        assert!(!second_only.contains("FIRST BODY"));
        assert!(second_only.contains("alt-e collapse artifacts"));

        let first = AnnotationSource::Walkthrough {
            step_id: "first".into(),
            part: 0,
        };
        state.diff_viewport.select_annotation(&session, first);
        assert_eq!(
            state.diff_viewport.toggle_annotation_artifacts(&session),
            Some(true)
        );
        let both =
            render_tui_text_with_state_and_keymap(&session, &Mode::Normal, &state, &keymap, 90, 34);
        assert!(both.contains("FIRST BODY"));
        assert!(both.contains("SECOND BODY"));
    }

    #[test]
    fn expansion_is_pruned_across_target_reset_even_when_card_id_is_reused() {
        let mut session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let target = crate::attention::target_for_diff(&files, "a.rs", Some(1), None).unwrap();
        let durable = super::super::ensure_tui_review_session(&mut session);
        durable.walkthroughs.push(Walkthrough {
            id: "walk".into(),
            steps: vec![WalkthroughStep {
                id: "reused".into(),
                author: Some(Identity::agent()),
                target: target.clone(),
                artifacts: vec![StepArtifact {
                    title: "artifact".into(),
                    kind: StepArtifactKind::Diagram,
                    body: "EXPANDED PAYLOAD".into(),
                }],
                ..Default::default()
            }],
            ..Default::default()
        });
        durable.attention_regions.push(AttentionRegion {
            target,
            salience: Salience::Spotlight,
            rationale: None,
            source: SalienceSource::Agent,
        });
        let owner = session
            .selected_walkthrough_card_owner(&session.sessions[0].walkthroughs[0].steps[0].target)
            .unwrap();
        session.select_diff_row(owner);
        let state = TuiState::default();
        assert_eq!(
            state.diff_viewport.toggle_annotation_artifacts(&session),
            Some(true)
        );
        assert!(
            render_tui_text_with_state(&session, &Mode::Normal, &state, 80, 24)
                .contains("EXPANDED PAYLOAD")
        );

        session.target.rev = "other-target".into();
        state.diff_viewport.reset(&session);
        session.target.rev = "@".into();
        let collapsed = render_tui_text_with_state(&session, &Mode::Normal, &state, 80, 24);
        assert!(!collapsed.contains("EXPANDED PAYLOAD"));
        assert!(collapsed.contains("expand 1 artifact"));

        assert_eq!(
            state.diff_viewport.toggle_annotation_artifacts(&session),
            Some(true)
        );
        let inner = Rect::new(0, 0, 78, 20);
        let refresh = state.diff_viewport.refresh_snapshot(&session, inner);
        state.diff_viewport.refreshed(refresh, &mut session, inner);
        assert!(
            !render_tui_text_with_state(&session, &Mode::Normal, &state, 80, 24)
                .contains("EXPANDED PAYLOAD")
        );
    }

    #[test]
    fn fingerprint_drift_suppresses_stale_walkthrough_narration() {
        let mut session = snapshot_session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let target = crate::attention::target_for_diff(&files, "a.rs", Some(1), None).unwrap();
        let durable = super::super::ensure_tui_review_session(&mut session);
        durable.walkthroughs.push(Walkthrough {
            id: "walk".into(),
            steps: vec![WalkthroughStep {
                id: "step".into(),
                author: Some(Identity::agent()),
                target: target.clone(),
                title: Some("Current narration".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        durable.attention_regions.push(AttentionRegion {
            target,
            salience: Salience::Spotlight,
            rationale: Some("stale rationale must disappear".into()),
            source: SalienceSource::Agent,
        });
        assert_eq!(
            selected_file_annotation_input(&session, &BTreeSet::new(), "E")
                .cards
                .len(),
            1
        );

        session.files[0].diff.fingerprint = "drifted".into();
        session.files[0].fingerprint = "drifted".into();
        assert!(
            selected_file_annotation_input(&session, &BTreeSet::new(), "E")
                .cards
                .is_empty()
        );
        assert!(
            !render_tui_text(&session, &Mode::Normal, 80, 20)
                .contains("stale rationale must disappear")
        );
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
