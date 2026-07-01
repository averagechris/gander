use std::{io, time::Duration};

use color_eyre::eyre::{Result, bail};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
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
    file_tree::{FlatTreeRow, FlatTreeRowKind},
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
    NextUnviewed,
    PreviousUnviewed,
    NextComment,
    PreviousComment,
    ScrollDown,
    ScrollUp,
    MarkViewed,
    ToggleViewed,
    MarkAllViewed,
    Comment,
    SubmitComment,
    CancelComment,
    DeleteChar,
}

enum Mode {
    Normal,
    CommentInput(String),
}

pub fn run(session: &mut ReviewSession, keybindings: &KeybindingsConfig) -> Result<()> {
    let keymap = KeyMap::try_from(keybindings)?;
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut mode = Mode::Normal;

    let result = run_loop(&mut terminal, session, &mut mode, &keymap);

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
                    && handle_normal_action(action, session, mode)
                {
                    break;
                }
            }
            Mode::CommentInput(buffer) => {
                let mut leave_comment_input = false;
                if let Some(action) = keymap.action_for(&key) {
                    leave_comment_input = handle_comment_action(action, session, buffer);
                } else if let KeyCode::Char(ch) = key.code {
                    buffer.push(ch);
                }
                if leave_comment_input {
                    *mode = Mode::Normal;
                }
            }
        }
    }
    Ok(())
}

fn handle_normal_action(action: Action, session: &mut ReviewSession, mode: &mut Mode) -> bool {
    match action {
        Action::Quit => return true,
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
        Action::NextUnviewed => session.move_to_unviewed(1),
        Action::PreviousUnviewed => session.move_to_unviewed(-1),
        Action::NextComment => session.move_to_comment(1),
        Action::PreviousComment => session.move_to_comment(-1),
        Action::ScrollDown => session.scroll_diff(12),
        Action::ScrollUp => session.scroll_diff(-12),
        Action::MarkViewed => session.mark_selected_viewed(),
        Action::ToggleViewed => session.toggle_viewed(),
        Action::MarkAllViewed => session.mark_all_viewed(),
        Action::Comment => *mode = Mode::CommentInput(String::new()),
        Action::SubmitComment | Action::CancelComment | Action::DeleteChar => {}
    }
    false
}

fn handle_comment_action(action: Action, session: &mut ReviewSession, buffer: &mut String) -> bool {
    match action {
        Action::CancelComment => return true,
        Action::SubmitComment => {
            let body = std::mem::take(buffer);
            session.add_comment(body);
            return true;
        }
        Action::DeleteChar => {
            buffer.pop();
        }
        _ => {}
    }
    false
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

    if let Mode::CommentInput(buffer) = mode {
        draw_comment_popup(frame, frame.area(), buffer);
    }
}

fn draw_files(frame: &mut ratatui::Frame<'_>, area: Rect, session: &ReviewSession) {
    let tree = session.file_tree();
    let items: Vec<ListItem<'_>> = tree
        .rows
        .iter()
        .map(|row| match &row.kind {
            FlatTreeRowKind::Directory => render_directory_row(row),
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

fn render_directory_row(row: &FlatTreeRow) -> ListItem<'static> {
    let indent = "  ".repeat(row.depth.min(8));
    ListItem::new(Line::from(vec![
        Span::raw(indent),
        Span::styled(row.stats.mark(), Style::default().fg(Color::Green)),
        Span::raw(" ▾ "),
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
            let style = diff_row_style(row.kind, selected);
            let lineno = row
                .new_lineno
                .or(row.old_lineno)
                .map(|n| format!("{n:>4}"))
                .unwrap_or_else(|| "    ".to_owned());
            let comment_mark = row
                .anchor
                .as_ref()
                .filter(|anchor| session.comments_for_anchor(anchor) > 0)
                .map(|_| "*")
                .unwrap_or(" ");

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

fn diff_row_style(kind: DiffRowKind, selected: bool) -> Style {
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
            "focus files · {down}/{up} file · {next_unviewed}/{previous_unviewed} unviewed · {next_comment}/{previous_comment} comments · {focus} diff · {mark} viewed · {toggle} toggle · {comment} comment · {quit} quit",
            down = keymap.hint(Action::MoveDown),
            up = keymap.hint(Action::MoveUp),
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
            "focus diff · {down}/{up} line · {next_unviewed}/{previous_unviewed} unviewed · {next_comment}/{previous_comment} comments · {focus} files · {comment} line comment · {scroll_down}/{scroll_up} scroll · {quit} quit",
            down = keymap.hint(Action::MoveDown),
            up = keymap.hint(Action::MoveUp),
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
            "type comment · {submit} save · {cancel} cancel",
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
        add_bindings(&mut bindings, Action::Comment, &config.comment)?;
        add_bindings(&mut bindings, Action::SubmitComment, &config.submit_comment)?;
        add_bindings(&mut bindings, Action::CancelComment, &config.cancel_comment)?;
        add_bindings(&mut bindings, Action::DeleteChar, &config.delete_char)?;
        Ok(Self { bindings })
    }
}

impl KeyMap {
    fn action_for(&self, key: &KeyEvent) -> Option<Action> {
        self.bindings
            .iter()
            .find(|binding| binding.key.code == key.code)
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
    let code = match normalized.as_str() {
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
        _ if raw.chars().count() == 1 => KeyCode::Char(raw.chars().next().unwrap()),
        _ => bail!("unsupported keybinding `{raw}`"),
    };
    Ok(KeyPress {
        code,
        label: raw.to_owned(),
    })
}

fn draw_comment_popup(frame: &mut ratatui::Frame<'_>, area: Rect, buffer: &str) {
    let popup = centered_rect(70, 20, area);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(buffer.to_owned())
            .block(Block::default().borders(Borders::ALL).title("comment"))
            .wrap(Wrap { trim: false }),
        popup,
    );
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
        assert_eq!(parse_key("N").unwrap().code, KeyCode::Char('N'));
        assert_eq!(parse_key("space").unwrap().code, KeyCode::Char(' '));
    }

    #[test]
    fn rejects_unsupported_key_names() {
        assert!(parse_key("ctrl-x").is_err());
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
}
