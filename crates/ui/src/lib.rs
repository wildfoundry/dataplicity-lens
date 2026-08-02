use std::{
    io::{self, Stdout},
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use lens_core::{select_processes, SnapshotSource, ViewOptions};
use lens_diagnostics::{analyse, Diagnostic, Severity};
use lens_model::{ProcessSnapshot, SortKey, SystemSnapshot};
use lens_output::{format_bytes, format_duration};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame, Terminal,
};

pub fn run<S: SnapshotSource>(
    mut source: S,
    mut options: ViewOptions,
    refresh_interval: Duration,
) -> Result<()> {
    let mut snapshot = source.refresh()?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(
        &mut terminal,
        &mut source,
        &mut snapshot,
        &mut options,
        refresh_interval,
    );

    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();

    result
}

fn run_loop<S: SnapshotSource>(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    source: &mut S,
    snapshot: &mut SystemSnapshot,
    options: &mut ViewOptions,
    refresh_interval: Duration,
) -> Result<()> {
    let mut selected = 0usize;
    let mut next_refresh = Instant::now() + refresh_interval;

    loop {
        let processes = select_processes(snapshot, options);
        selected = selected.min(processes.len().saturating_sub(1));
        let findings = analyse(snapshot);

        terminal.draw(|frame| draw(frame, snapshot, &processes, &findings, options, selected))?;

        let timeout = next_refresh.saturating_duration_since(Instant::now());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Down | KeyCode::Char('j') => {
                        selected = (selected + 1).min(processes.len().saturating_sub(1));
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        selected = selected.saturating_sub(1);
                    }
                    KeyCode::Char('c') => options.sort_key = SortKey::Cpu,
                    KeyCode::Char('m') => options.sort_key = SortKey::Memory,
                    KeyCode::Char('p') => options.sort_key = SortKey::Pid,
                    KeyCode::Char('n') => options.sort_key = SortKey::Name,
                    KeyCode::Char('d') => options.descending = !options.descending,
                    _ => {}
                }
            }
        }

        if Instant::now() >= next_refresh {
            *snapshot = source.refresh()?;
            next_refresh = Instant::now() + refresh_interval;
        }
    }
}

fn draw(
    frame: &mut Frame,
    snapshot: &SystemSnapshot,
    processes: &[ProcessSnapshot],
    findings: &[Diagnostic],
    options: &ViewOptions,
    selected: usize,
) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(frame.size());

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " LENS TOP ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "  {}  up {}  CPU {:.1}%  MEM {:.1}%  LOAD {:.2} {:.2} {:.2}",
            snapshot.hostname,
            format_duration(snapshot.uptime_secs),
            snapshot.cpu_usage_percent,
            snapshot.memory.used_percent(),
            snapshot.load_average.one,
            snapshot.load_average.five,
            snapshot.load_average.fifteen,
        )),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, areas[0]);

    let diagnostic_lines: Vec<Line> = findings
        .iter()
        .take(2)
        .map(|finding| {
            Line::from(vec![
                Span::styled(
                    format!(" {} ", severity_label(finding.severity)),
                    severity_style(finding.severity),
                ),
                Span::styled(
                    format!(" {}", finding.title),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(" - {}", finding.detail)),
            ])
        })
        .collect();
    let diagnostics = Paragraph::new(diagnostic_lines).block(
        Block::default()
            .title(" Diagnostics ")
            .borders(Borders::ALL),
    );
    frame.render_widget(diagnostics, areas[1]);

    let visible_rows = areas[2].height.saturating_sub(3) as usize;
    let first_visible = selected.saturating_sub(visible_rows.saturating_sub(1));
    let rows = processes
        .iter()
        .enumerate()
        .skip(first_visible)
        .take(visible_rows)
        .map(|(index, process)| {
            let command = if process.command.is_empty() {
                &process.name
            } else {
                &process.command
            };
            let style = if index == selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(process.pid.to_string()),
                Cell::from(format!("{:.1}", process.cpu_percent)),
                Cell::from(format_bytes(process.memory_bytes)),
                Cell::from(process.status.clone()),
                Cell::from(command.to_owned()),
            ])
            .style(style)
        });

    let direction = if options.descending { "desc" } else { "asc" };
    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(11),
            Constraint::Length(13),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(["PID", "CPU %", "MEM", "STATE", "COMMAND"]).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .column_spacing(1)
    .block(
        Block::default()
            .title(format!(
                " Processes {}/{} - sort {} {} ",
                processes.len(),
                snapshot.processes.len(),
                sort_label(options.sort_key),
                direction
            ))
            .borders(Borders::ALL),
    );
    frame.render_widget(table, areas[2]);

    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" q ", key_style()),
        Span::raw("quit  "),
        Span::styled(" j/k ", key_style()),
        Span::raw("move  "),
        Span::styled(" c/m/p/n ", key_style()),
        Span::raw("sort  "),
        Span::styled(" d ", key_style()),
        Span::raw("direction"),
    ]));
    frame.render_widget(footer, areas[3]);
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "INFO",
        Severity::Warning => "WARN",
        Severity::Critical => "CRIT",
    }
}

fn severity_style(severity: Severity) -> Style {
    let colour = match severity {
        Severity::Info => Color::Green,
        Severity::Warning => Color::Yellow,
        Severity::Critical => Color::Red,
    };
    Style::default()
        .fg(Color::Black)
        .bg(colour)
        .add_modifier(Modifier::BOLD)
}

fn sort_label(sort_key: SortKey) -> &'static str {
    match sort_key {
        SortKey::Cpu => "cpu",
        SortKey::Memory => "memory",
        SortKey::Pid => "pid",
        SortKey::Name => "name",
        SortKey::Runtime => "runtime",
    }
}

fn key_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}
