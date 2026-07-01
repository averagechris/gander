use std::{io, time::Duration};

use color_eyre::eyre::{Context, Result, bail};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
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
    jj::{JjCommand, ReviewTarget},
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
    NextUnviewed,
    PreviousUnviewed,
    NextComment,
    PreviousComment,
    ScrollDown,
    ScrollUp,
    MarkViewed,
    ToggleViewed,
    MarkAllViewed,
    ToggleFold,
    CollapseFold,
    ExpandFold,
    RangeComment,
    CancelRangeComment,
    Comment,
    SubmitComment,
    CancelComment,
    InsertNewline,
    DeleteChar,
}

enum Mode {
    Normal,
    CommentInput(CommentEditor),
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

#[derive(Debug, Clone)]
struct ReviewLoader {
    ignore_globs: Vec<String>,
    generated_matcher: GeneratedMatcher,
}

pub fn run(
    session: &mut ReviewSession,
    keybindings: &KeybindingsConfig,
    ignore_globs: Vec<String>,
    generated_matcher: GeneratedMatcher,
) -> Result<()> {
    let keymap = KeyMap::try_from(keybindings)?;
    let review_loader = ReviewLoader {
        ignore_globs,
        generated_matcher,
    };
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut mode = Mode::Normal;

    let result = run_loop(&mut terminal, session, &mut mode, &keymap, &review_loader);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    session: &mut ReviewSession,
    mode: &mut Mode,
    keymap: &KeyMap,
    review_loader: &ReviewLoader,
) -> Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, session, mode, keymap))?;

        if !event::poll(Duration::from_millis(150))? {
            continue;
        }

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match mode {
            Mode::Normal => {
                if let Some(action) = keymap.action_for(&key)
                    && handle_normal_action(action, session, mode, review_loader)?
                {
                    break;
                }
            }
            Mode::CommentInput(editor) => {
                let mut leave_comment_input = false;
                if let Some(action) = keymap.comment_action_for(&key) {
                    leave_comment_input = handle_comment_action(action, session, editor);
                } else {
                    handle_comment_key(key, editor);
                }
                if leave_comment_input {
                    *mode = Mode::Normal;
                }
            }
        }
    }
    Ok(())
}

fn handle_normal_action(
    action: Action,
    session: &mut ReviewSession,
    mode: &mut Mode,
    review_loader: &ReviewLoader,
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
        Action::CompareTrunk => review_loader.load(session, ReviewTarget::trunk_to_current())?,
        Action::CompareParent => review_loader.load(session, ReviewTarget::parent_to_current())?,
        Action::NextUnviewed => session.move_to_unviewed(1),
        Action::PreviousUnviewed => session.move_to_unviewed(-1),
        Action::NextComment => session.move_to_comment(1),
        Action::PreviousComment => session.move_to_comment(-1),
        Action::ScrollDown => session.scroll_diff(12),
        Action::ScrollUp => session.scroll_diff(-12),
        Action::MarkViewed => session.mark_selected_viewed(),
        Action::ToggleViewed => session.toggle_viewed(),
        Action::MarkAllViewed => session.mark_all_viewed(),
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
        Action::Comment => *mode = Mode::CommentInput(CommentEditor::default()),
        Action::SubmitComment
        | Action::CancelComment
        | Action::InsertNewline
        | Action::DeleteChar => {}
    }
    Ok(false)
}

impl ReviewLoader {
    fn load(&self, session: &mut ReviewSession, target: ReviewTarget) -> Result<()> {
        let diff_text = JjCommand::new(session.repo.clone(), target.clone())
            .diff()
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

fn handle_comment_action(
    action: Action,
    session: &mut ReviewSession,
    editor: &mut CommentEditor,
) -> bool {
    match action {
        Action::CancelComment => return true,
        Action::SubmitComment => {
            let body = std::mem::take(editor).into_text();
            session.add_comment(body);
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

fn draw(frame: &mut ratatui::Frame<'_>, session: &ReviewSession, mode: &Mode, keymap: &KeyMap) {
    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(2)])
        .split(frame.area());
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(44), Constraint::Min(40)])
        .split(main[0]);

    draw_files(frame, body[0], session);
    draw_diff(frame, body[1], session);
    draw_footer(frame, main[1], session, mode, keymap);

    if let Mode::CommentInput(editor) = mode {
        draw_comment_popup(frame, frame.area(), editor);
    }
}

fn draw_files(frame: &mut ratatui::Frame<'_>, area: Rect, session: &ReviewSession) {
    let tree = session.file_tree();
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
    if session.selected_file().is_none() {
        frame.render_widget(Paragraph::new("No changed files"), area);
        return;
    }

    let rows = session.diff_rows_for_selected_file();
    let lines: Vec<_> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let selected = session.focus == Focus::Diff && session.diff_cursor == index;
            let in_range = session.diff_row_in_active_range(index);
            let style = diff_row_style(row.kind, selected, in_range);
            let lineno = row
                .new_lineno
                .or(row.old_lineno)
                .map(|n| format!("{n:>4}"))
                .unwrap_or_else(|| "    ".to_owned());
            let comment_mark = row
                .anchor
                .as_ref()
                .filter(|anchor| session.comments_for_diff_row_anchor(anchor) > 0)
                .map(|_| "*")
                .unwrap_or(if in_range { "|" } else { " " });

            match row.kind {
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
                DiffRowKind::DiffLine(_) => Line::from(vec![
                    Span::styled(comment_mark, Style::default().fg(Color::Yellow)),
                    Span::styled(lineno, Style::default().fg(Color::DarkGray)),
                    Span::raw(" "),
                    Span::styled(row.prefix, style),
                    Span::raw(" "),
                    Span::styled(row.text.clone(), style),
                ]),
            }
        })
        .collect();

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("diff"))
        .scroll((session.diff_scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
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
) {
    let mode_text = match mode {
        Mode::Normal if session.focus == Focus::Files => format!(
            "{} · focus files · {down}/{up} tree · {fold} fold · {trunk}/{parent} base · {next_unviewed}/{previous_unviewed} unviewed · {next_comment}/{previous_comment} comments · {focus} diff · {mark} viewed · {toggle} toggle · {comment} comment · {quit} quit",
            session.target,
            down = keymap.hint(Action::MoveDown),
            up = keymap.hint(Action::MoveUp),
            fold = keymap.hint(Action::ToggleFold),
            trunk = keymap.hint(Action::CompareTrunk),
            parent = keymap.hint(Action::CompareParent),
            next_unviewed = keymap.hint(Action::NextUnviewed),
            previous_unviewed = keymap.hint(Action::PreviousUnviewed),
            next_comment = keymap.hint(Action::NextComment),
            previous_comment = keymap.hint(Action::PreviousComment),
            focus = keymap.hint(Action::ToggleFocus),
            mark = keymap.hint(Action::MarkViewed),
            toggle = keymap.hint(Action::ToggleViewed),
            comment = keymap.hint(Action::Comment),
            quit = keymap.hint(Action::Quit),
        ),
        Mode::Normal => format!(
            "{} · focus diff{} · {down}/{up} line · {range} range · {cancel_range} cancel · {trunk}/{parent} base · {next_unviewed}/{previous_unviewed} unviewed · {next_comment}/{previous_comment} comments · {focus} files · {comment} comment · {scroll_down}/{scroll_up} scroll · {quit} quit",
            session.target,
            if session.has_active_diff_range() {
                " (range active)"
            } else {
                ""
            },
            down = keymap.hint(Action::MoveDown),
            up = keymap.hint(Action::MoveUp),
            range = keymap.hint(Action::RangeComment),
            cancel_range = keymap.hint(Action::CancelRangeComment),
            trunk = keymap.hint(Action::CompareTrunk),
            parent = keymap.hint(Action::CompareParent),
            next_unviewed = keymap.hint(Action::NextUnviewed),
            previous_unviewed = keymap.hint(Action::PreviousUnviewed),
            next_comment = keymap.hint(Action::NextComment),
            previous_comment = keymap.hint(Action::PreviousComment),
            focus = keymap.hint(Action::ToggleFocus),
            comment = keymap.hint(Action::Comment),
            scroll_down = keymap.hint(Action::ScrollDown),
            scroll_up = keymap.hint(Action::ScrollUp),
            quit = keymap.hint(Action::Quit),
        ),
        Mode::CommentInput(_) => format!(
            "type comment · {newline} newline · {submit} save · {cancel} cancel",
            newline = keymap.hint(Action::InsertNewline),
            submit = keymap.hint(Action::SubmitComment),
            cancel = keymap.hint(Action::CancelComment),
        ),
    };
    frame.render_widget(
        Paragraph::new(format!("{}\n{}", session.summary_line(), mode_text))
            .style(Style::default().fg(Color::DarkGray)),
        area,
    );
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
