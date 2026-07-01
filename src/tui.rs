use std::{io, time::Duration};

use color_eyre::eyre::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
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
    file_tree::{FlatTreeRow, FlatTreeRowKind},
};

enum Mode {
    Normal,
    CommentInput(String),
}

pub fn run(session: &mut ReviewSession) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut mode = Mode::Normal;

    let result = run_loop(&mut terminal, session, &mut mode);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    session: &mut ReviewSession,
    mode: &mut Mode,
) -> Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, session, mode))?;

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
            Mode::Normal => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('j') | KeyCode::Down => match session.focus {
                    Focus::Files => session.move_selection(1),
                    Focus::Diff => session.move_diff_cursor(1),
                },
                KeyCode::Char('k') | KeyCode::Up => match session.focus {
                    Focus::Files => session.move_selection(-1),
                    Focus::Diff => session.move_diff_cursor(-1),
                },
                KeyCode::Tab => session.toggle_focus(),
                KeyCode::Char('g') => session.diff_scroll = 0,
                KeyCode::Char('G') => session.diff_scroll = u16::MAX / 2,
                KeyCode::Char('n') => session.move_to_unviewed(1),
                KeyCode::Char('N') => session.move_to_unviewed(-1),
                KeyCode::Char('m') => session.move_to_comment(1),
                KeyCode::Char('M') => session.move_to_comment(-1),
                KeyCode::Char('d') | KeyCode::PageDown => session.scroll_diff(12),
                KeyCode::Char('u') | KeyCode::PageUp => session.scroll_diff(-12),
                KeyCode::Enter => session.mark_selected_viewed(),
                KeyCode::Char('v') => session.toggle_viewed(),
                KeyCode::Char('a') => session.mark_all_viewed(),
                KeyCode::Char('c') => *mode = Mode::CommentInput(String::new()),
                _ => {}
            },
            Mode::CommentInput(buffer) => match key.code {
                KeyCode::Esc => *mode = Mode::Normal,
                KeyCode::Enter => {
                    let body = std::mem::take(buffer);
                    session.add_comment(body);
                    *mode = Mode::Normal;
                }
                KeyCode::Backspace => {
                    buffer.pop();
                }
                KeyCode::Char(ch) => buffer.push(ch),
                _ => {}
            },
        }
    }
    Ok(())
}

fn draw(frame: &mut ratatui::Frame<'_>, session: &ReviewSession, mode: &Mode) {
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
    draw_footer(frame, main[1], session, mode);

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

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect, session: &ReviewSession, mode: &Mode) {
    let mode_text = match mode {
        Mode::Normal if session.focus == Focus::Files => {
            "focus files · j/k file · n/N unviewed · m/M comments · tab diff · enter viewed · v toggle · c file comment · q quit"
        }
        Mode::Normal => {
            "focus diff · j/k line · n/N unviewed · m/M comments · tab files · c line comment · u/d scroll · q quit"
        }
        Mode::CommentInput(_) => "type comment · enter save · esc cancel",
    };
    frame.render_widget(
        Paragraph::new(format!("{}\n{}", session.summary_line(), mode_text))
            .style(Style::default().fg(Color::DarkGray)),
        area,
    );
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
