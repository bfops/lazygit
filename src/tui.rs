use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Terminal;

use crate::diff_model::{apply_hunk, hunks, DiffLine};
use crate::github::Gh;
use crate::state::ReviewSession;

type Term = Terminal<CrosstermBackend<Stdout>>;

#[derive(Debug, Default)]
struct App {
    file_index: usize,
    hunk_index: usize,
    message: String,
}

pub fn run(mut session: ReviewSession, gh: Gh) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let result = run_loop(&mut terminal, &mut session, &gh);
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn run_loop(terminal: &mut Term, session: &mut ReviewSession, gh: &Gh) -> Result<()> {
    let mut app = App::default();
    loop {
        terminal.draw(|frame| draw(frame, session, &app))?;
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if handle_key(key, session, gh, &mut app)? {
                    return Ok(());
                }
            }
        }
    }
}

fn handle_key(key: KeyEvent, session: &mut ReviewSession, gh: &Gh, app: &mut App) -> Result<bool> {
    match key.code {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Down | KeyCode::Char('j') => {
            if app.file_index + 1 < session.files.len() {
                app.file_index += 1;
                app.hunk_index = 0;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.file_index > 0 {
                app.file_index -= 1;
                app.hunk_index = 0;
            }
        }
        KeyCode::Char('n') => {
            let count = current_hunks(session, app).len();
            if app.hunk_index + 1 < count {
                app.hunk_index += 1;
            }
        }
        KeyCode::Char('p') => {
            if app.hunk_index > 0 {
                app.hunk_index -= 1;
            }
        }
        KeyCode::Char('a') => accept_selected_hunk(session, gh, app)?,
        KeyCode::Char('A') => accept_all_hunks(session, gh, app)?,
        KeyCode::Char('r') => {
            session.refresh_files(gh)?;
            app.hunk_index = 0;
            app.message = "refreshed from GitHub".into();
        }
        _ => {}
    }
    Ok(false)
}

fn accept_selected_hunk(session: &mut ReviewSession, gh: &Gh, app: &mut App) -> Result<()> {
    if session.files.is_empty() {
        return Ok(());
    }
    let h = current_hunks(session, app);
    if h.is_empty() {
        app.message = "file is already caught up".into();
        return Ok(());
    }
    let index = app.hunk_index.min(h.len() - 1);
    let reviewed = session.files[app.file_index].reviewed.clone();
    let next = apply_hunk(&reviewed, &h[index]);
    session.accept_file_content(app.file_index, next, gh)?;
    app.hunk_index = 0;
    app.message = "accepted hunk".into();
    Ok(())
}

fn accept_all_hunks(session: &mut ReviewSession, gh: &Gh, app: &mut App) -> Result<()> {
    if session.files.is_empty() {
        return Ok(());
    }
    let current = session.files[app.file_index].current.clone();
    session.accept_file_content(app.file_index, current, gh)?;
    app.hunk_index = 0;
    app.message = "accepted file".into();
    Ok(())
}

fn current_hunks(session: &ReviewSession, app: &App) -> Vec<crate::diff_model::Hunk> {
    session
        .files
        .get(app.file_index)
        .map(|file| hunks(&file.reviewed, &file.current))
        .unwrap_or_default()
}

fn draw(frame: &mut ratatui::Frame, session: &ReviewSession, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(32), Constraint::Percentage(68)])
        .split(frame.area());

    let files: Vec<ListItem> = session
        .files
        .iter()
        .enumerate()
        .map(|(idx, file)| {
            let caught_up = file.reviewed == file.current;
            let prefix = if caught_up { "[x]" } else { "[ ]" };
            let style = if idx == app.file_index {
                Style::default().fg(Color::Black).bg(Color::White)
            } else {
                Style::default()
            };
            ListItem::new(format!("{prefix} {}", file.meta.path)).style(style)
        })
        .collect();
    let file_list = List::new(files).block(Block::default().title("Files").borders(Borders::ALL));
    frame.render_widget(file_list, chunks[0]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(chunks[1]);
    let diff = diff_lines(session, app);
    frame.render_widget(
        Paragraph::new(diff).block(
            Block::default()
                .title("Reviewed -> Current")
                .borders(Borders::ALL),
        ),
        right[0],
    );
    let help = format!(
        "j/k move  n/p hunk  a accept hunk  A accept file  r refresh  q quit  {}",
        app.message
    );
    frame.render_widget(
        Paragraph::new(help).block(Block::default().title("Keys").borders(Borders::ALL)),
        right[1],
    );
}

fn diff_lines(session: &ReviewSession, app: &App) -> Vec<Line<'static>> {
    let Some(file) = session.files.get(app.file_index) else {
        return vec![Line::from("No files")];
    };
    let h = hunks(&file.reviewed, &file.current);
    if h.is_empty() {
        return vec![Line::from("File caught up to latest PR content")];
    }
    let mut lines = Vec::new();
    for (hunk_idx, hunk) in h.iter().enumerate() {
        let header_style = if hunk_idx == app.hunk_index {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        lines.push(Line::from(Span::styled(
            format!("@@ hunk {} @@", hunk_idx + 1),
            header_style,
        )));
        for line in &hunk.lines {
            match line {
                DiffLine::Equal(text) => {
                    lines.push(Line::from(format!(" {}", text.trim_end_matches('\n'))))
                }
                DiffLine::Delete(text) => lines.push(Line::from(Span::styled(
                    format!("-{}", text.trim_end_matches('\n')),
                    Style::default().fg(Color::Red),
                ))),
                DiffLine::Insert(text) => lines.push(Line::from(Span::styled(
                    format!("+{}", text.trim_end_matches('\n')),
                    Style::default().fg(Color::Green),
                ))),
            }
        }
    }
    lines
}
