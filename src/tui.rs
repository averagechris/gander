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

use crate::{app::ReviewSession, diff::DiffLineKind, syntax};

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
                KeyCode::Char('j') | KeyCode::Down => session.move_selection(1),
                KeyCode::Char('k') | KeyCode::Up => session.move_selection(-1),
                KeyCode::Char('g') => session.diff_scroll = 0,
                KeyCode::Char('G') => session.diff_scroll = u16::MAX / 2,
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
        .constraints([Constraint::Length(38), Constraint::Min(40)])
        .split(main[0]);

    draw_files(frame, body[0], session);
    draw_diff(frame, body[1], session);
    draw_footer(frame, main[1], session, mode);

    if let Mode::CommentInput(buffer) = mode {
        draw_comment_popup(frame, frame.area(), buffer);
    }
}

fn draw_files(frame: &mut ratatui::Frame<'_>, area: Rect, session: &ReviewSession) {
    let items: Vec<ListItem<'_>> = session
        .files
        .iter()
        .map(|file| {
            let mark = if file.viewed { "✓" } else { "•" };
            let style = if file.viewed {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(vec![
                Span::styled(mark, Style::default().fg(Color::Green)),
                Span::raw(" "),
                Span::styled(
                    format!("{:>7}", file.status),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(" "),
                Span::styled(file.path.clone(), style),
            ]))
        })
        .collect();

    let mut state = ListState::default().with_selected(Some(session.selected));
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("files"))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_diff(frame: &mut ratatui::Frame<'_>, area: Rect, session: &ReviewSession) {
    let Some(file) = session.selected_file() else {
        frame.render_widget(Paragraph::new("No changed files"), area);
        return;
    };

    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            &file.path,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  +{} -{}", file.additions, file.deletions)),
    ]));

    let added_source = file
        .diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| matches!(line.kind, DiffLineKind::Added | DiffLineKind::Context))
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(summary) = syntax::summarize(&file.path, &added_source) {
        lines.push(Line::from(Span::styled(
            format!(
                "tree-sitter: {} root={} errors={}",
                summary.language, summary.root_kind, summary.has_error
            ),
            Style::default().fg(Color::Magenta),
        )));
    }

    for hunk in &file.diff.hunks {
        lines.push(Line::from(Span::styled(
            hunk.header.clone(),
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )));
        for line in &hunk.lines {
            let (prefix, style) = match line.kind {
                DiffLineKind::Context => (" ", Style::default().fg(Color::Gray)),
                DiffLineKind::Added => ("+", Style::default().fg(Color::Green)),
                DiffLineKind::Removed => ("-", Style::default().fg(Color::Red)),
                DiffLineKind::Meta => ("\\", Style::default().fg(Color::DarkGray)),
            };
            let lineno = line
                .new_lineno
                .or(line.old_lineno)
                .map(|n| format!("{n:>4}"))
                .unwrap_or_else(|| "    ".to_owned());
            lines.push(Line::from(vec![
                Span::styled(lineno, Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::styled(prefix, style),
                Span::raw(" "),
                Span::styled(line.text.clone(), style),
            ]));
        }
    }

    if file.diff.hunks.is_empty() {
        lines.push(Line::from(file.diff.raw.clone()));
    }

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("diff"))
        .scroll((session.diff_scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect, session: &ReviewSession, mode: &Mode) {
    let mode_text = match mode {
        Mode::Normal => {
            "j/k move · enter viewed · v toggle · c comment · a all viewed · u/d scroll · q quit"
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
