use std::{io, time::Duration};

use color_eyre::eyre::{Context, Result, bail};
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
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    app::{DiffRowKind, Focus, ReviewSession},
    config::KeybindingsConfig,
    diff::DiffSet,
    file_tree::{FlatTreeRow, FlatTreeRowKind},
    generated::GeneratedMatcher,
    jj::{JjBackend, JjChangeSummary, ReviewTarget},
    syntax::{HighlightKind, SyntaxSpan, SyntaxThemeConfig},
};

#[derive(Debug, Clone)]
pub struct KeyMap {
    bindings: Vec<KeyBinding>,
}

#[derive(Debug, Clone)]
struct KeyBinding {
    key: KeyPress,
    action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyPress {
    code: KeyCode,
    modifiers: KeyModifiers,
    label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Quit,
    MoveDown,
    MoveUp,
    ToggleFocus,
    DiffTop,
    DiffBottom,
    CompareTrunk,
    CompareParent,
    TargetChooser,
    TargetPickerMoveDown,
    TargetPickerMoveUp,
    NextUnviewed,
    PreviousUnviewed,
    NextComment,
    PreviousComment,
    ScrollDown,
    ScrollUp,
    MarkViewed,
    ToggleViewed,
    MarkAllViewed,
    ToggleGenerated,
    ToggleFold,
    CollapseFold,
    ExpandFold,
    RangeComment,
    CancelRangeComment,
    Comment,
    EditComment,
    DeleteComment,
    SubmitComment,
    CancelComment,
    InsertNewline,
    DeleteChar,
}

enum Mode {
    Normal,
    TargetChooser(TargetChooserState),
    CommentInput {
        editor: CommentEditor,
        target: CommentInputTarget,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetChooserState {
    rows: Vec<JjChangeSummary>,
    filtered: Vec<usize>,
    selected: usize,
    query: String,
    current_base: String,
    current_tip: String,
    selecting: TargetPickerSide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetPickerSide {
    Base,
    Tip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CommentInputTarget {
    New,
    Edit { id: String },
}

#[derive(Debug, Default)]
struct TuiState {
    diff_drag: Option<DiffDrag>,
    notice: Option<UiNotice>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UiLayout {
    files: Rect,
    diff: Rect,
    footer: Rect,
}

#[derive(Debug, Clone, Default)]
struct CommentEditor {
    text: String,
    cursor: usize,
}

impl CommentEditor {
    fn insert_char(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let Some((previous, ch)) = self.text[..self.cursor].char_indices().last() else {
            return;
        };
        self.text.drain(previous..self.cursor);
        self.cursor -= ch.len_utf8();
    }

    fn move_left(&mut self) {
        if let Some((previous, _)) = self.text[..self.cursor].char_indices().last() {
            self.cursor = previous;
        }
    }

    fn move_right(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let ch = self.text[self.cursor..].chars().next().unwrap();
        self.cursor += ch.len_utf8();
    }

    fn line_col(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        let line = before.chars().filter(|ch| *ch == '\n').count();
        let col = before
            .rsplit_once('\n')
            .map(|(_, tail)| tail.chars().count())
            .unwrap_or_else(|| before.chars().count());
        (line, col)
    }

    fn set_line_col(&mut self, target_line: usize, target_col: usize) {
        let mut line = 0;
        let mut col = 0;
        for (index, ch) in self.text.char_indices() {
            if line == target_line && col == target_col {
                self.cursor = index;
                return;
            }
            if ch == '\n' {
                if line == target_line {
                    self.cursor = index;
                    return;
                }
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        self.cursor = self.text.len();
    }

    fn move_up(&mut self) {
        let (line, col) = self.line_col();
        if line > 0 {
            self.set_line_col(line - 1, col);
        }
    }

    fn move_down(&mut self) {
        let (line, col) = self.line_col();
        if line + 1 < self.text.lines().count().max(1) {
            self.set_line_col(line + 1, col);
        }
    }

    fn into_text(self) -> String {
        self.text
    }
}

impl TargetChooserState {
    fn new(rows: Vec<JjChangeSummary>, current_base: &str, current_tip: &str) -> Self {
        let selected = rows
            .iter()
            .position(|row| row.matches_rev(current_base))
            .unwrap_or(0);
        let filtered = (0..rows.len()).collect();
        Self {
            rows,
            filtered,
            selected,
            query: String::new(),
            current_base: current_base.to_owned(),
            current_tip: current_tip.to_owned(),
            selecting: TargetPickerSide::Base,
        }
    }

    fn target(&self) -> Option<ReviewTarget> {
        let selected = self.rows.get(*self.filtered.get(self.selected)?)?;
        Some(match self.selecting {
            TargetPickerSide::Base => {
                ReviewTarget::new(selected.change_id.clone(), self.current_tip.clone())
            }
            TargetPickerSide::Tip => {
                ReviewTarget::new(self.current_base.clone(), selected.change_id.clone())
            }
        })
    }

    fn toggle_side(&mut self) {
        self.selecting = match self.selecting {
            TargetPickerSide::Base => TargetPickerSide::Tip,
            TargetPickerSide::Tip => TargetPickerSide::Base,
        };
        let current = match self.selecting {
            TargetPickerSide::Base => &self.current_base,
            TargetPickerSide::Tip => &self.current_tip,
        };
        if let Some(selected) = self.selected_index_for_rev(current) {
            self.selected = selected;
        }
    }

    fn selected_index_for_rev(&self, rev: &str) -> Option<usize> {
        if rev == "@" && !self.filtered.is_empty() {
            return Some(0);
        }
        self.filtered
            .iter()
            .position(|row_index| self.rows[*row_index].matches_rev(rev))
    }

    fn tip_matches_row(&self, row: &JjChangeSummary, row_index: usize) -> bool {
        if self.current_tip == "@" {
            row_index == 0
        } else {
            row.matches_rev(&self.current_tip)
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.filtered.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    fn select_first(&mut self) {
        self.selected = 0;
    }

    fn select_last(&mut self) {
        self.selected = self.filtered.len().saturating_sub(1);
    }

    fn push_query_char(&mut self, ch: char) {
        self.query.push(ch);
        self.apply_filter();
    }

    fn pop_query_char(&mut self) {
        self.query.pop();
        self.apply_filter();
    }

    fn apply_filter(&mut self) {
        self.filtered = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| fuzzy_matches(row, &self.query).then_some(index))
            .collect();
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }
}

impl TargetPickerSide {
    fn label(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Tip => "tip",
        }
    }
}

impl JjChangeSummary {
    fn matches_rev(&self, rev: &str) -> bool {
        self.change_id == rev
            || self
                .bookmarks
                .split_whitespace()
                .any(|bookmark| bookmark.trim_end_matches('*') == rev)
    }
}

fn fuzzy_matches(row: &JjChangeSummary, query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let haystack = format!("{} {} {}", row.change_id, row.bookmarks, row.description);
    fuzzy_contains(&haystack.to_ascii_lowercase(), &query.to_ascii_lowercase())
}

fn fuzzy_contains(haystack: &str, needle: &str) -> bool {
    let mut haystack_chars = haystack.chars();
    needle.chars().all(|needle_char| {
        haystack_chars
            .by_ref()
            .any(|haystack_char| haystack_char == needle_char)
    })
}

struct ReviewLoader<'a> {
    ignore_globs: Vec<String>,
    generated_matcher: GeneratedMatcher,
    jj: &'a dyn JjBackend,
}

pub fn run(
    session: &mut ReviewSession,
    keybindings: &KeybindingsConfig,
    ignore_globs: Vec<String>,
    generated_matcher: GeneratedMatcher,
    jj: &dyn JjBackend,
) -> Result<()> {
    let keymap = KeyMap::try_from(keybindings)?;
    let review_loader = ReviewLoader {
        ignore_globs,
        generated_matcher,
        jj,
    };
    enable_raw_mode()?;
    // Render the interactive UI to stderr so stdout remains clean for artifacts.
    // This lets `jj-change-viewer > review.md` capture only the post-quit artifact.
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;
    let mut mode = Mode::Normal;

    let mut tui_state = TuiState::default();
    let result = run_loop(
        &mut terminal,
        session,
        &mut mode,
        &keymap,
        &review_loader,
        &mut tui_state,
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

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    session: &mut ReviewSession,
    mode: &mut Mode,
    keymap: &KeyMap,
    review_loader: &ReviewLoader<'_>,
    tui_state: &mut TuiState,
) -> Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, session, mode, keymap, tui_state.notice.as_ref()))?;

        if !event::poll(Duration::from_millis(150))? {
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
    }
    Ok(())
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
        Mode::TargetChooser(chooser) => {
            if handle_target_chooser_key(key, chooser, session, keymap, review_loader, tui_state) {
                *mode = Mode::Normal;
            }
        }
        Mode::CommentInput { editor, target } => {
            let mut leave_comment_input = false;
            if let Some(action) = keymap.comment_action_for(&key) {
                leave_comment_input = handle_comment_action(action, session, editor, target);
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
        Action::DiffBottom => session.diff_scroll = u16::MAX / 2,
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
        Action::NextUnviewed => session.move_to_unviewed(1),
        Action::PreviousUnviewed => session.move_to_unviewed(-1),
        Action::NextComment => session.move_to_comment(1),
        Action::PreviousComment => session.move_to_comment(-1),
        Action::ScrollDown => session.scroll_diff(12),
        Action::ScrollUp => session.scroll_diff(-12),
        Action::MarkViewed => session.mark_selected_viewed(),
        Action::ToggleViewed => session.toggle_viewed(),
        Action::MarkAllViewed => session.mark_all_viewed(),
        Action::ToggleGenerated => session.toggle_generated_visibility(),
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
        Action::RangeComment => {
            if session.focus == Focus::Diff {
                session.toggle_diff_range_selection();
            }
        }
        Action::CancelRangeComment => session.clear_diff_range_selection(),
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
        KeyCode::Char('g') => {
            chooser.select_first();
            false
        }
        KeyCode::Char('G') => {
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
        KeyCode::Char(ch) if key.modifiers.is_empty() => {
            chooser.push_query_char(ch);
            false
        }
        _ => false,
    }
}

impl ReviewLoader<'_> {
    fn base_candidates(&self, session: &ReviewSession) -> Result<Vec<JjChangeSummary>> {
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
        session.annotate_generated_where(|file| self.generated_matcher.is_match(&file.path));
        session.apply_viewed_state();
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

fn draw(
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

fn ui_layout(area: Rect) -> UiLayout {
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

fn inner_bordered(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

fn point_in_rect(x: u16, y: u16, rect: Rect) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

fn row_in_inner(y: u16, inner: Rect) -> Option<usize> {
    (y >= inner.y && y < inner.y.saturating_add(inner.height)).then_some((y - inner.y) as usize)
}

fn handle_mouse_event(
    mouse: MouseEvent,
    terminal_size: ratatui::prelude::Size,
    session: &mut ReviewSession,
    mode: &mut Mode,
    tui_state: &mut TuiState,
) {
    if matches!(mode, Mode::CommentInput { .. }) {
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
    layout: UiLayout,
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
    layout: UiLayout,
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

fn comment_summary_line(comment: &crate::state::Comment) -> Line<'static> {
    let summary = comment
        .body
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("(empty comment)")
        .trim();
    Line::from(vec![
        Span::styled("      ↳ ", Style::default().fg(Color::Yellow)),
        Span::styled(
            format!("{} ", comment.id),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(summary.to_owned(), Style::default().fg(Color::Yellow)),
    ])
}

fn diff_text_spans<'a>(
    row: &'a crate::app::DiffRow,
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
        DiffRowKind::DiffLine(crate::diff::DiffLineKind::Context) => {
            Style::default().fg(Color::Gray)
        }
        DiffRowKind::DiffLine(crate::diff::DiffLineKind::Added) => {
            Style::default().fg(Color::Green)
        }
        DiffRowKind::DiffLine(crate::diff::DiffLineKind::Removed) => {
            Style::default().fg(Color::Red)
        }
        DiffRowKind::DiffLine(crate::diff::DiffLineKind::Meta) => {
            Style::default().fg(Color::DarkGray)
        }
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

impl TryFrom<&KeybindingsConfig> for KeyMap {
    type Error = color_eyre::Report;

    fn try_from(config: &KeybindingsConfig) -> Result<Self> {
        let mut bindings = Vec::new();
        add_bindings(&mut bindings, Action::Quit, &config.quit)?;
        add_bindings(&mut bindings, Action::MoveDown, &config.move_down)?;
        add_bindings(&mut bindings, Action::MoveUp, &config.move_up)?;
        add_bindings(&mut bindings, Action::ToggleFocus, &config.toggle_focus)?;
        add_bindings(&mut bindings, Action::DiffTop, &config.diff_top)?;
        add_bindings(&mut bindings, Action::DiffBottom, &config.diff_bottom)?;
        add_bindings(&mut bindings, Action::CompareTrunk, &config.compare_trunk)?;
        add_bindings(&mut bindings, Action::CompareParent, &config.compare_parent)?;
        add_bindings(&mut bindings, Action::TargetChooser, &config.target_chooser)?;
        add_bindings(
            &mut bindings,
            Action::TargetPickerMoveDown,
            &config.target_picker_down,
        )?;
        add_bindings(
            &mut bindings,
            Action::TargetPickerMoveUp,
            &config.target_picker_up,
        )?;
        add_bindings(&mut bindings, Action::NextUnviewed, &config.next_unviewed)?;
        add_bindings(
            &mut bindings,
            Action::PreviousUnviewed,
            &config.previous_unviewed,
        )?;
        add_bindings(&mut bindings, Action::NextComment, &config.next_comment)?;
        add_bindings(
            &mut bindings,
            Action::PreviousComment,
            &config.previous_comment,
        )?;
        add_bindings(&mut bindings, Action::ScrollDown, &config.scroll_down)?;
        add_bindings(&mut bindings, Action::ScrollUp, &config.scroll_up)?;
        add_bindings(&mut bindings, Action::MarkViewed, &config.mark_viewed)?;
        add_bindings(&mut bindings, Action::ToggleViewed, &config.toggle_viewed)?;
        add_bindings(
            &mut bindings,
            Action::MarkAllViewed,
            &config.mark_all_viewed,
        )?;
        add_bindings(
            &mut bindings,
            Action::ToggleGenerated,
            &config.toggle_generated,
        )?;
        add_bindings(&mut bindings, Action::ToggleFold, &config.toggle_fold)?;
        add_bindings(&mut bindings, Action::CollapseFold, &config.collapse_fold)?;
        add_bindings(&mut bindings, Action::ExpandFold, &config.expand_fold)?;
        add_bindings(&mut bindings, Action::RangeComment, &config.range_comment)?;
        add_bindings(
            &mut bindings,
            Action::CancelRangeComment,
            &config.cancel_range_comment,
        )?;
        add_bindings(&mut bindings, Action::Comment, &config.comment)?;
        add_bindings(&mut bindings, Action::EditComment, &config.edit_comment)?;
        add_bindings(&mut bindings, Action::DeleteComment, &config.delete_comment)?;
        add_bindings(&mut bindings, Action::SubmitComment, &config.submit_comment)?;
        add_bindings(&mut bindings, Action::CancelComment, &config.cancel_comment)?;
        add_bindings(&mut bindings, Action::InsertNewline, &config.insert_newline)?;
        add_bindings(&mut bindings, Action::DeleteChar, &config.delete_char)?;
        Ok(Self { bindings })
    }
}

impl KeyMap {
    fn action_for(&self, key: &KeyEvent) -> Option<Action> {
        self.bindings
            .iter()
            .find(|binding| binding.key.matches(key))
            .map(|binding| binding.action)
    }

    fn comment_action_for(&self, key: &KeyEvent) -> Option<Action> {
        self.bindings
            .iter()
            .find(|binding| {
                binding.key.matches(key)
                    && matches!(
                        binding.action,
                        Action::SubmitComment
                            | Action::CancelComment
                            | Action::InsertNewline
                            | Action::DeleteChar
                    )
            })
            .map(|binding| binding.action)
    }

    fn target_picker_action_for(&self, key: &KeyEvent) -> Option<Action> {
        self.bindings
            .iter()
            .find(|binding| {
                binding.key.matches(key)
                    && matches!(
                        binding.action,
                        Action::TargetPickerMoveDown | Action::TargetPickerMoveUp
                    )
            })
            .map(|binding| binding.action)
    }

    fn hint(&self, action: Action) -> &str {
        self.bindings
            .iter()
            .find(|binding| binding.action == action)
            .map(|binding| binding.key.label.as_str())
            .unwrap_or("?")
    }
}

impl KeyPress {
    fn matches(&self, key: &KeyEvent) -> bool {
        self.code == key.code && self.modifiers == key.modifiers
    }
}

fn add_bindings(bindings: &mut Vec<KeyBinding>, action: Action, keys: &[String]) -> Result<()> {
    for key in keys {
        bindings.push(KeyBinding {
            key: parse_key(key)?,
            action,
        });
    }
    Ok(())
}

fn parse_key(raw: &str) -> Result<KeyPress> {
    let normalized = raw.trim().to_ascii_lowercase();
    if matches!(normalized.as_str(), "page-up" | "page-down") {
        return Ok(KeyPress {
            code: if normalized == "page-up" {
                KeyCode::PageUp
            } else {
                KeyCode::PageDown
            },
            modifiers: KeyModifiers::empty(),
            label: raw.to_owned(),
        });
    }
    let parts: Vec<_> = normalized.split(['-', '+']).collect();
    let (modifiers, key_name) = parse_key_parts(&parts, raw)?;
    let code = match key_name {
        "esc" | "escape" => KeyCode::Esc,
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backspace" | "bs" => KeyCode::Backspace,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pageup" | "page-up" | "pgup" => KeyCode::PageUp,
        "pagedown" | "page-down" | "pgdn" => KeyCode::PageDown,
        "space" => KeyCode::Char(' '),
        _ if key_name.chars().count() == 1 => {
            KeyCode::Char(if modifiers.is_empty() && raw.trim().chars().count() == 1 {
                raw.trim().chars().next().unwrap()
            } else {
                key_name.chars().next().unwrap()
            })
        }
        _ => bail!("unsupported keybinding `{raw}`"),
    };
    Ok(KeyPress {
        code,
        modifiers,
        label: raw.to_owned(),
    })
}

fn parse_key_parts<'a>(parts: &'a [&'a str], raw: &str) -> Result<(KeyModifiers, &'a str)> {
    let Some((key_name, modifiers)) = parts.split_last() else {
        bail!("unsupported keybinding `{raw}`");
    };
    let mut parsed = KeyModifiers::empty();
    for modifier in modifiers {
        match *modifier {
            "ctrl" | "control" => parsed |= KeyModifiers::CONTROL,
            "alt" => parsed |= KeyModifiers::ALT,
            "shift" => parsed |= KeyModifiers::SHIFT,
            _ => bail!("unsupported keybinding `{raw}`"),
        }
    }
    Ok((parsed, key_name))
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
    use std::{cell::RefCell, path::Path};

    use crate::{
        diff::DiffSet,
        jj::{JjBackend, ReviewTarget},
        state::{Comment, ReviewState},
        syntax::{HighlightKind, SyntaxConfig, SyntaxSpan, SyntaxThemeConfig},
    };

    struct MockJjBackend {
        calls: RefCell<Vec<ReviewTarget>>,
        diff_text: Result<String, String>,
        summaries: Vec<JjChangeSummary>,
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
    }

    fn snapshot_session(diff_text: &str) -> ReviewSession {
        let diff = DiffSet::parse(diff_text).unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.syntax = SyntaxConfig {
            enabled: false,
            ..SyntaxConfig::default()
        };
        session
    }

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
        let backend = MockJjBackend {
            calls: RefCell::new(Vec::new()),
            diff_text: Ok(r#"diff --git a/new.rs b/new.rs
--- a/new.rs
+++ b/new.rs
@@ -1 +1 @@
-old
+new
"#
            .to_owned()),
            summaries: Vec::new(),
        };
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
        let backend = MockJjBackend {
            calls: RefCell::new(Vec::new()),
            diff_text: Err("boom".to_owned()),
            summaries: Vec::new(),
        };
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
    fn parses_named_and_character_keys() {
        assert_eq!(parse_key("down").unwrap().code, KeyCode::Down);
        assert_eq!(parse_key("pagedown").unwrap().code, KeyCode::PageDown);
        assert_eq!(parse_key("page-down").unwrap().code, KeyCode::PageDown);
        assert_eq!(parse_key("N").unwrap().code, KeyCode::Char('N'));
        assert_eq!(parse_key("space").unwrap().code, KeyCode::Char(' '));
    }

    #[test]
    fn parses_modified_keys() {
        let key = parse_key("ctrl-s").unwrap();

        assert_eq!(key.code, KeyCode::Char('s'));
        assert_eq!(key.modifiers, KeyModifiers::CONTROL);
    }

    #[test]
    fn rejects_unsupported_key_names() {
        assert!(parse_key("hyper-space").is_err());
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
    fn configured_key_overrides_default_action() {
        let config = KeybindingsConfig {
            move_down: vec!["s".to_owned()],
            ..KeybindingsConfig::default()
        };
        let keymap = KeyMap::try_from(&config).unwrap();

        let key = KeyEvent::from(KeyCode::Char('s'));

        assert_eq!(keymap.action_for(&key), Some(Action::MoveDown));
        assert_eq!(keymap.hint(Action::MoveDown), "s");
    }

    #[test]
    fn default_fold_keybindings_map_to_actions() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char(' '))),
            Some(Action::ToggleFold)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Left)),
            Some(Action::CollapseFold)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Right)),
            Some(Action::ExpandFold)
        );
    }

    #[test]
    fn default_generated_keybinding_maps_to_action() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('h'))),
            Some(Action::ToggleGenerated)
        );
    }

    #[test]
    fn default_comment_edit_delete_keybindings_map_to_actions() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('e'))),
            Some(Action::EditComment)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('x'))),
            Some(Action::DeleteComment)
        );
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
            &target
        ));

        assert_eq!(session.comments.len(), 1);
        assert_eq!(session.comments[0].body, "new body");
    }

    #[test]
    fn comment_mode_uses_comment_actions_only() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.comment_action_for(&KeyEvent::from(KeyCode::Down)),
            None
        );
        assert_eq!(
            keymap.comment_action_for(&KeyEvent::from(KeyCode::Enter)),
            Some(Action::InsertNewline)
        );
        assert_eq!(
            keymap.comment_action_for(&KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            Some(Action::SubmitComment)
        );
    }

    #[test]
    fn comment_editor_inserts_multiline_text() {
        let mut editor = CommentEditor::default();
        for ch in "hello".chars() {
            editor.insert_char(ch);
        }
        editor.insert_newline();
        for ch in "world".chars() {
            editor.insert_char(ch);
        }

        assert_eq!(editor.text, "hello\nworld");
        assert_eq!(editor.line_col(), (1, 5));
    }

    #[test]
    fn comment_editor_backspace_merges_lines() {
        let mut editor = CommentEditor {
            text: "hello\nworld".to_owned(),
            cursor: "hello\n".len(),
        };

        editor.backspace();

        assert_eq!(editor.text, "helloworld");
        assert_eq!(editor.line_col(), (0, 5));
    }

    #[test]
    fn default_compare_keybindings_map_to_actions() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('t'))),
            Some(Action::CompareTrunk)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('p'))),
            Some(Action::CompareParent)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('b'))),
            Some(Action::TargetChooser)
        );
    }

    #[test]
    fn default_target_picker_keybindings_map_to_actions() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.target_picker_action_for(&KeyEvent::from(KeyCode::Down)),
            Some(Action::TargetPickerMoveDown)
        );
        assert_eq!(
            keymap.target_picker_action_for(&KeyEvent::new(
                KeyCode::Char('j'),
                KeyModifiers::CONTROL
            )),
            Some(Action::TargetPickerMoveDown)
        );
        assert_eq!(
            keymap.target_picker_action_for(&KeyEvent::from(KeyCode::Up)),
            Some(Action::TargetPickerMoveUp)
        );
        assert_eq!(
            keymap.target_picker_action_for(&KeyEvent::new(
                KeyCode::Char('k'),
                KeyModifiers::CONTROL
            )),
            Some(Action::TargetPickerMoveUp)
        );
    }

    #[test]
    fn target_chooser_loads_selected_base_to_current_tip() {
        let mut session = snapshot_session("");
        let backend = MockJjBackend {
            calls: RefCell::new(Vec::new()),
            diff_text: Ok(String::new()),
            summaries: Vec::new(),
        };
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
    fn target_chooser_selects_current_base_when_visible() {
        let chooser = TargetChooserState::new(
            vec![
                JjChangeSummary {
                    change_id: "abc".to_owned(),
                    bookmarks: String::new(),
                    description: String::new(),
                },
                JjChangeSummary {
                    change_id: "def".to_owned(),
                    bookmarks: "trunk".to_owned(),
                    description: String::new(),
                },
            ],
            "trunk",
            "@",
        );

        assert_eq!(chooser.selected, 1);
        assert_eq!(chooser.target(), Some(ReviewTarget::new("def", "@")));
    }

    #[test]
    fn target_chooser_can_switch_to_tip_selection() {
        let mut chooser = TargetChooserState::new(
            vec![
                JjChangeSummary {
                    change_id: "base".to_owned(),
                    bookmarks: String::new(),
                    description: String::new(),
                },
                JjChangeSummary {
                    change_id: "tip".to_owned(),
                    bookmarks: "feature".to_owned(),
                    description: String::new(),
                },
            ],
            "base",
            "feature",
        );

        chooser.toggle_side();

        assert_eq!(chooser.selecting, TargetPickerSide::Tip);
        assert_eq!(chooser.selected, 1);
        assert_eq!(chooser.target(), Some(ReviewTarget::new("base", "tip")));
    }

    #[test]
    fn target_chooser_fuzzy_filters_rows() {
        let mut chooser = TargetChooserState::new(
            vec![
                JjChangeSummary {
                    change_id: "abc".to_owned(),
                    bookmarks: "main".to_owned(),
                    description: "feature work".to_owned(),
                },
                JjChangeSummary {
                    change_id: "def".to_owned(),
                    bookmarks: "topic".to_owned(),
                    description: "bug fix".to_owned(),
                },
            ],
            "abc",
            "@",
        );

        for ch in "tp".chars() {
            chooser.push_query_char(ch);
        }

        assert_eq!(chooser.filtered, vec![1]);
        assert_eq!(chooser.target(), Some(ReviewTarget::new("def", "@")));
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

    #[test]
    fn default_range_keybindings_map_to_actions() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('r'))),
            Some(Action::RangeComment)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            Some(Action::CancelRangeComment)
        );
    }
}
