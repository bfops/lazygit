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
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Terminal;

use crate::diff_model::{apply_hunk, hunks, DiffLine};
use crate::github::Gh;
use crate::jobs::{JobEvent, JobWorker};
use crate::log_buffer::LogBuffer;
use crate::prefs::{DiffViewMode, Preferences};
use crate::state::{ReviewFile, ReviewSession};

type Term = Terminal<CrosstermBackend<Stdout>>;
const DIFF_VERTICAL_SCROLL_STEP: u16 = 3;

#[derive(Debug)]
struct App {
    file_index: usize,
    hunk_index: usize,
    diff_vertical_offset: u16,
    message: String,
    prefs: Preferences,
    logs: LogBuffer,
    refresh_pending: bool,
    pending_marks: usize,
}

impl App {
    fn new(prefs: Preferences, logs: LogBuffer) -> Self {
        Self {
            file_index: 0,
            hunk_index: 0,
            diff_vertical_offset: 0,
            message: String::new(),
            prefs,
            logs,
            refresh_pending: false,
            pending_marks: 0,
        }
    }
}

pub fn run(mut session: ReviewSession, gh: Gh, prefs: Preferences, logs: LogBuffer) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let result = run_loop(&mut terminal, &mut session, &gh, prefs, logs);
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

fn run_loop(
    terminal: &mut Term,
    session: &mut ReviewSession,
    gh: &Gh,
    prefs: Preferences,
    logs: LogBuffer,
) -> Result<()> {
    let mut app = App::new(prefs, logs);
    let worker = JobWorker::start(
        session.root().to_path_buf(),
        session.manifest.pr.clone(),
        gh.clone(),
    );
    loop {
        drain_job_events(session, &worker, &mut app)?;
        terminal.draw(|frame| draw(frame, session, &app))?;
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if handle_key(key, session, &worker, &mut app)? {
                    return Ok(());
                }
            }
        }
    }
}

fn handle_key(
    key: KeyEvent,
    session: &mut ReviewSession,
    worker: &JobWorker,
    app: &mut App,
) -> Result<bool> {
    match key.code {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Down => {
            if app.file_index + 1 < session.files.len() {
                app.file_index += 1;
                app.hunk_index = 0;
                reset_diff_scroll(app);
            }
        }
        KeyCode::Up => {
            if app.file_index > 0 {
                app.file_index -= 1;
                app.hunk_index = 0;
                reset_diff_scroll(app);
            }
        }
        KeyCode::Char(']') | KeyCode::Char('n') => {
            let count = current_hunks(session, app).len();
            if app.hunk_index + 1 < count {
                app.hunk_index += 1;
                reset_diff_scroll(app);
            }
        }
        KeyCode::Char('[') | KeyCode::Char('p') => {
            if app.hunk_index > 0 {
                app.hunk_index -= 1;
                reset_diff_scroll(app);
            }
        }
        KeyCode::Char('j') | KeyCode::Char('d') | KeyCode::PageDown => {
            scroll_diff_down(app);
        }
        KeyCode::Char('k') | KeyCode::Char('u') | KeyCode::PageUp => {
            scroll_diff_up(app);
        }
        KeyCode::Home => reset_diff_scroll(app),
        KeyCode::End => {
            app.diff_vertical_offset = u16::MAX;
        }
        KeyCode::Char('a') => accept_selected_hunk(session, worker, app)?,
        KeyCode::Char('A') => accept_all_hunks(session, worker, app)?,
        KeyCode::Char('r') => {
            if app.refresh_pending {
                app.message = "refresh already in progress".into();
                app.logs.record("refresh already in progress");
            } else {
                worker.refresh_files()?;
                app.refresh_pending = true;
                app.message = "refresh queued".into();
                app.logs.record("refresh queued");
            }
        }
        KeyCode::Char('w') => {
            app.prefs.toggle_diff_mode();
            app.prefs.save()?;
            app.message = format!("diff mode: {}", mode_label(&app.prefs));
            app.logs.record(format!("changed {}", app.message));
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if app.prefs.diff_view_mode == DiffViewMode::Scroll {
                app.prefs.diff_horizontal_offset =
                    app.prefs.diff_horizontal_offset.saturating_add(4);
                app.prefs.save()?;
            } else {
                scroll_diff_down(app);
            }
        }
        KeyCode::Left | KeyCode::Char('h') => {
            if app.prefs.diff_view_mode == DiffViewMode::Scroll {
                app.prefs.diff_horizontal_offset =
                    app.prefs.diff_horizontal_offset.saturating_sub(4);
                app.prefs.save()?;
            } else {
                scroll_diff_up(app);
            }
        }
        _ => {}
    }
    Ok(false)
}

fn accept_selected_hunk(
    session: &mut ReviewSession,
    worker: &JobWorker,
    app: &mut App,
) -> Result<()> {
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
    let outcome = session.accept_file_content_local(app.file_index, next)?;
    let remaining = current_hunks(session, app).len();
    clamp_hunk_index(app, remaining);
    reset_diff_scroll(app);
    app.message = accepted_hunk_message(remaining);
    app.logs.record(format!(
        "accepted hunk in {}",
        session.files[app.file_index].meta.path
    ));
    enqueue_mark_viewed_if_needed(session, worker, app, outcome)?;
    Ok(())
}

fn accept_all_hunks(session: &mut ReviewSession, worker: &JobWorker, app: &mut App) -> Result<()> {
    if session.files.is_empty() {
        return Ok(());
    }
    let current = session.files[app.file_index].current.clone();
    let outcome = session.accept_file_content_local(app.file_index, current)?;
    app.hunk_index = 0;
    reset_diff_scroll(app);
    app.message = "accepted file".into();
    app.logs.record(format!(
        "accepted file {}",
        session.files[app.file_index].meta.path
    ));
    enqueue_mark_viewed_if_needed(session, worker, app, outcome)?;
    Ok(())
}

fn enqueue_mark_viewed_if_needed(
    session: &ReviewSession,
    worker: &JobWorker,
    app: &mut App,
    outcome: crate::state::AcceptedFileOutcome,
) -> Result<()> {
    if outcome.should_mark_viewed {
        worker.mark_file_viewed(session.manifest.pr.id.clone(), outcome.path.clone())?;
        app.pending_marks += 1;
        app.logs
            .record(format!("queued GitHub viewed mark for {}", outcome.path));
    }
    Ok(())
}

fn drain_job_events(session: &mut ReviewSession, worker: &JobWorker, app: &mut App) -> Result<()> {
    for event in worker.drain_events() {
        match event {
            JobEvent::Progress(message) => app.logs.record(message),
            JobEvent::RefreshFinished(Ok(files)) => {
                session.apply_refreshed_files(files)?;
                app.refresh_pending = false;
                app.hunk_index = 0;
                reset_diff_scroll(app);
                app.message = "refresh complete".into();
                app.logs.record("refresh complete");
            }
            JobEvent::RefreshFinished(Err(error)) => {
                app.refresh_pending = false;
                app.message = "refresh failed".into();
                app.logs.record(format!("refresh failed: {error}"));
            }
            JobEvent::MarkFileViewedFinished { path, result } => {
                app.pending_marks = app.pending_marks.saturating_sub(1);
                match result {
                    Ok(()) => app.logs.record(format!("marked viewed in GitHub: {path}")),
                    Err(error) => app.logs.record(format!(
                        "failed to mark viewed in GitHub for {path}: {error}"
                    )),
                }
            }
        }
    }
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
            let hunk_label = hunk_count_label(remaining_hunk_count(file));
            let style = if idx == app.file_index {
                Style::default().fg(Color::Black).bg(Color::White)
            } else {
                Style::default()
            };
            ListItem::new(format!(
                "{prefix} {}{}",
                file_list_path_label(file),
                hunk_label
            ))
            .style(style)
        })
        .collect();
    let file_list = List::new(files).block(Block::default().title("Files").borders(Borders::ALL));
    frame.render_widget(file_list, chunks[0]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(6),
            Constraint::Length(3),
        ])
        .split(chunks[1]);
    let diff = diff_lines(session, app);
    let mut diff_paragraph = Paragraph::new(diff)
        .block(
            Block::default()
                .title(format!(
                    "Reviewed -> Current y={}",
                    app.diff_vertical_offset
                ))
                .borders(Borders::ALL),
        )
        .scroll((app.diff_vertical_offset, 0));
    if app.prefs.diff_view_mode == DiffViewMode::Wrap {
        diff_paragraph = diff_paragraph.wrap(Wrap { trim: false });
    }
    frame.render_widget(diff_paragraph, right[0]);
    frame.render_widget(
        Paragraph::new(log_lines(app)).block(Block::default().title("Log").borders(Borders::ALL)),
        right[1],
    );
    let help = format!(
        "Up/Down file  [/]/n/p hunk  hjkl diff scroll  a accept  A accept file  w wrap/scroll  r refresh  q quit  {}  {}  {}",
        mode_label(&app.prefs),
        job_label(app),
        app.message
    );
    frame.render_widget(
        Paragraph::new(help).block(Block::default().title("Keys").borders(Borders::ALL)),
        right[2],
    );
}

fn job_label(app: &App) -> String {
    let refresh = if app.refresh_pending {
        "refresh=pending"
    } else {
        "refresh=idle"
    };
    format!("{refresh} marks={}", app.pending_marks)
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
    let total = h.len();
    let index = app.hunk_index.min(total - 1);
    lines.push(Line::from(Span::styled(
        remaining_hunks_label(total),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    for line in &h[index].lines {
        match line {
            DiffLine::Equal(text) => lines.push(diff_line(" ", text, Style::default(), app)),
            DiffLine::Delete(text) => {
                lines.push(diff_line("-", text, Style::default().fg(Color::Red), app))
            }
            DiffLine::Insert(text) => {
                lines.push(diff_line("+", text, Style::default().fg(Color::Green), app))
            }
        }
    }
    lines
}

fn remaining_hunk_count(file: &ReviewFile) -> usize {
    hunks(&file.reviewed, &file.current).len()
}

fn file_list_path_label(file: &ReviewFile) -> String {
    if let Some(previous_path) = &file.meta.previous_path {
        if previous_path != &file.meta.path {
            return format!("{previous_path} -> {}", file.meta.path);
        }
    }
    file.meta.path.clone()
}

fn hunk_count_label(count: usize) -> String {
    match count {
        0 => String::new(),
        1 => " (1 hunk)".into(),
        count => format!(" ({count} hunks)"),
    }
}

fn remaining_hunks_label(count: usize) -> String {
    match count {
        1 => "1 hunk remaining".into(),
        count => format!("{count} hunks remaining"),
    }
}

fn clamp_hunk_index(app: &mut App, remaining_count: usize) {
    if remaining_count == 0 {
        app.hunk_index = 0;
    } else if app.hunk_index >= remaining_count {
        app.hunk_index = remaining_count - 1;
    }
}

fn reset_diff_scroll(app: &mut App) {
    app.diff_vertical_offset = 0;
}

fn scroll_diff_down(app: &mut App) {
    app.diff_vertical_offset = app
        .diff_vertical_offset
        .saturating_add(DIFF_VERTICAL_SCROLL_STEP);
}

fn scroll_diff_up(app: &mut App) {
    app.diff_vertical_offset = app
        .diff_vertical_offset
        .saturating_sub(DIFF_VERTICAL_SCROLL_STEP);
}

fn accepted_hunk_message(remaining_count: usize) -> String {
    match remaining_count {
        0 => "accepted hunk, file caught up".into(),
        1 => "accepted hunk, 1 left".into(),
        count => format!("accepted hunk, {count} left"),
    }
}

fn diff_line(prefix: &str, text: &str, style: Style, app: &App) -> Line<'static> {
    let text = format!("{prefix}{}", display_line_text(text));
    Line::from(Span::styled(scroll_text(&text, app), style))
}

fn display_line_text(text: &str) -> &str {
    text.trim_end_matches(['\r', '\n'])
}

fn scroll_text(text: &str, app: &App) -> String {
    if app.prefs.diff_view_mode == DiffViewMode::Wrap {
        return text.to_owned();
    }
    slice_from_char_offset(text, app.prefs.diff_horizontal_offset)
}

fn slice_from_char_offset(text: &str, offset: usize) -> String {
    text.chars().skip(offset).collect()
}

fn mode_label(prefs: &Preferences) -> String {
    match prefs.diff_view_mode {
        DiffViewMode::Wrap => "mode=wrap".into(),
        DiffViewMode::Scroll => format!("mode=scroll x={}", prefs.diff_horizontal_offset),
    }
}

fn log_lines(app: &App) -> Vec<Line<'static>> {
    app.logs
        .entries()
        .into_iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(Line::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slicing_past_end_returns_empty_string() {
        assert_eq!(slice_from_char_offset("abc", 99), "");
    }

    #[test]
    fn slicing_uses_character_offsets() {
        assert_eq!(slice_from_char_offset("abcdef", 2), "cdef");
    }

    #[test]
    fn display_line_text_strips_line_endings_only() {
        assert_eq!(display_line_text("abc\n"), "abc");
        assert_eq!(display_line_text("abc\r\n"), "abc");
        assert_eq!(display_line_text("abc"), "abc");
        assert_eq!(display_line_text("\n"), "");
        assert_eq!(display_line_text("abc  \n"), "abc  ");
    }

    #[test]
    fn diff_line_has_no_embedded_line_terminators() {
        let logs = LogBuffer::new(5);
        let app = App::new(Preferences::default(), logs);
        let line = diff_line("+", "abc\r\n", Style::default(), &app);

        let rendered = line.spans[0].content.as_ref();
        assert_eq!(rendered, "+abc");
        assert!(!rendered.contains('\n'));
        assert!(!rendered.contains('\r'));
    }

    #[test]
    fn mode_label_reports_scroll_offset() {
        let prefs = Preferences {
            diff_view_mode: DiffViewMode::Scroll,
            diff_horizontal_offset: 8,
        };

        assert_eq!(mode_label(&prefs), "mode=scroll x=8");
    }

    #[test]
    fn job_label_reports_pending_counts() {
        let logs = LogBuffer::new(5);
        let mut app = App::new(Preferences::default(), logs);
        app.refresh_pending = true;
        app.pending_marks = 2;

        assert_eq!(job_label(&app), "refresh=pending marks=2");
    }

    #[test]
    fn hunk_count_label_formats_counts() {
        assert_eq!(hunk_count_label(0), "");
        assert_eq!(hunk_count_label(1), " (1 hunk)");
        assert_eq!(hunk_count_label(2), " (2 hunks)");
    }

    #[test]
    fn remaining_hunks_label_formats_counts() {
        assert_eq!(remaining_hunks_label(1), "1 hunk remaining");
        assert_eq!(remaining_hunks_label(2), "2 hunks remaining");
    }

    #[test]
    fn clamp_hunk_index_keeps_valid_index() {
        let logs = LogBuffer::new(5);
        let mut app = App::new(Preferences::default(), logs);
        app.hunk_index = 1;

        clamp_hunk_index(&mut app, 3);

        assert_eq!(app.hunk_index, 1);
    }

    #[test]
    fn clamp_hunk_index_moves_to_last_remaining_hunk() {
        let logs = LogBuffer::new(5);
        let mut app = App::new(Preferences::default(), logs);
        app.hunk_index = 3;

        clamp_hunk_index(&mut app, 2);

        assert_eq!(app.hunk_index, 1);
    }

    #[test]
    fn clamp_hunk_index_resets_when_no_hunks_remain() {
        let logs = LogBuffer::new(5);
        let mut app = App::new(Preferences::default(), logs);
        app.hunk_index = 3;

        clamp_hunk_index(&mut app, 0);

        assert_eq!(app.hunk_index, 0);
    }

    #[test]
    fn accepted_hunk_message_reports_remaining_work() {
        assert_eq!(accepted_hunk_message(0), "accepted hunk, file caught up");
        assert_eq!(accepted_hunk_message(1), "accepted hunk, 1 left");
        assert_eq!(accepted_hunk_message(2), "accepted hunk, 2 left");
    }

    #[test]
    fn file_list_label_formats_renamed_files() {
        let file = ReviewFile {
            meta: crate::state::FileState {
                path: "new/path.rs".into(),
                previous_path: Some("old/path.rs".into()),
                change_type: "RENAMED".into(),
                reviewed_hash: String::new(),
                current_hash: None,
                base_path_used: Some("old/path.rs".into()),
                base_ref_oid_used: Some("base".into()),
                initial_reviewed_hash: Some(String::new()),
                unsupported: false,
            },
            reviewed: String::new(),
            current: String::new(),
        };

        assert_eq!(file_list_path_label(&file), "old/path.rs -> new/path.rs");
    }

    #[test]
    fn scroll_diff_down_increases_vertical_offset() {
        let logs = LogBuffer::new(5);
        let mut app = App::new(Preferences::default(), logs);

        scroll_diff_down(&mut app);

        assert_eq!(app.diff_vertical_offset, DIFF_VERTICAL_SCROLL_STEP);
    }

    #[test]
    fn scroll_diff_up_saturates_at_zero() {
        let logs = LogBuffer::new(5);
        let mut app = App::new(Preferences::default(), logs);

        scroll_diff_up(&mut app);

        assert_eq!(app.diff_vertical_offset, 0);
    }

    #[test]
    fn reset_diff_scroll_clears_vertical_offset() {
        let logs = LogBuffer::new(5);
        let mut app = App::new(Preferences::default(), logs);
        app.diff_vertical_offset = 42;

        reset_diff_scroll(&mut app);

        assert_eq!(app.diff_vertical_offset, 0);
    }
}
