use lens_core::{GroupMode, SortDirection, SortKey};
use lens_model::{EntityId, Process, Severity};
use lens_output::{format_bytes, format_duration, format_rate, truncate};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Sparkline, Table,
        TableState, Wrap,
    },
};

use crate::{TerminalCapabilities, app::{App, InputMode, View}};

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    if area.width < 58 || area.height < 12 {
        draw_too_small(frame, area);
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(4),
            Constraint::Min(4),
            Constraint::Length(if app.error.is_some() { 2 } else { 1 }),
        ])
        .split(area);
    draw_header(frame, rows[0], app);
    draw_summary(frame, rows[1], app);
    match app.view {
        View::List => draw_processes(frame, rows[2], app),
        View::Detail => draw_detail(frame, rows[2], app),
    }
    draw_footer(frame, rows[3], app);

    if app.show_help {
        draw_help(frame, area, app.capabilities);
    } else if app.show_sort {
        draw_sort(frame, area, app);
    } else if let Some(mode) = app.input_mode {
        draw_input(frame, area, app, mode);
    }
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let state = if app.paused { "paused" } else { "running" };
    let title = Line::from(vec![
        Span::styled(" Dataplicity Lens ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("· "),
        Span::styled(&app.snapshot.host.hostname, info_style(app.capabilities)),
        Span::raw(format!(
            "    {:.1}s · {} · group {} · sort {} {}",
            app.interval().as_secs_f64(),
            state,
            app.group.label(),
            app.sort_key.label(),
            direction_symbol(app.sort_direction, app.capabilities),
        )),
    ]);
    frame.render_widget(Paragraph::new(title), area);
}

fn draw_summary(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(area);
    let cpu_data = app.host_cpu_history();
    let memory_data = app.memory_history();
    let cpu = Sparkline::default()
        .block(Block::default().borders(Borders::ALL).title(format!(
            " CPU {:>5.1}% ",
            app.snapshot.host.cpu_percent
        )))
        .data(&cpu_data)
        .max(100)
        .style(info_style(app.capabilities));
    frame.render_widget(cpu, columns[0]);

    let memory = Sparkline::default()
        .block(Block::default().borders(Borders::ALL).title(format!(
            " Memory {:>5.1}% ",
            app.snapshot.host.memory.used_percent()
        )))
        .data(&memory_data)
        .max(100)
        .style(attention_style(app.capabilities));
    frame.render_widget(memory, columns[1]);

    let counts = app.snapshot.host.process_counts;
    let summary = vec![
        Line::from(format!(
            "Load  {:.2}  {:.2}  {:.2}",
            app.snapshot.host.load.one,
            app.snapshot.host.load.five,
            app.snapshot.host.load.fifteen
        )),
        Line::from(format!(
            "Processes {}  running {}  zombies {}",
            counts.total, counts.running, counts.zombie
        )),
    ];
    frame.render_widget(
        Paragraph::new(summary).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Findings {} ", app.snapshot.findings.len())),
        ),
        columns[2],
    );
}

fn draw_processes(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let visible = app.visible();
    let wide = area.width >= 115;
    let medium = area.width >= 88;
    let mut rows = Vec::with_capacity(visible.len());
    for item in &visible {
        let process = &app.snapshot.processes[item.index];
        let prefix = if item.depth == 0 {
            String::new()
        } else if app.capabilities.unicode {
            format!("{}└─", "  ".repeat(item.depth.saturating_sub(1)))
        } else {
            format!("{}+-", "  ".repeat(item.depth.saturating_sub(1)))
        };
        let group = item
            .group_label
            .as_ref()
            .map_or_else(String::new, |label| format!("[{label}] "));
        let name = format!("{group}{prefix}{}", process.name);
        let mut cells = vec![
            Cell::from(process.pid.0.to_string()),
            Cell::from(truncate(&name, 30)),
            Cell::from(truncate(&process.user.display_name(), 14)),
            Cell::from(format!("{:.1}", process.cpu_percent)),
            Cell::from(format!("{:.1}", process.memory_percent)),
            Cell::from(format_bytes(process.rss_bytes)),
            Cell::from(process.state.short()),
        ];
        if medium {
            cells.push(Cell::from(process.threads.to_string()));
            cells.push(Cell::from(format_duration(process.runtime_seconds)));
        }
        if wide {
            cells.push(Cell::from(format_rate(process.io.read_bytes_per_second)));
            cells.push(Cell::from(format_rate(process.io.write_bytes_per_second)));
            cells.push(Cell::from(truncate(
                process
                    .service
                    .as_ref()
                    .map(|service| service.name.as_str())
                    .or_else(|| process.cgroup.as_ref().map(|cgroup| cgroup.path.as_str()))
                    .unwrap_or("-"),
                24,
            )));
        }
        let row_style = if process.state == lens_model::ProcessState::Zombie {
            critical_style(app.capabilities)
        } else {
            Style::default()
        };
        rows.push(Row::new(cells).style(row_style));
    }

    let mut header = vec!["PID", "PROCESS", "USER", "CPU%", "MEM%", "RSS", "ST"];
    let mut widths = vec![
        Constraint::Length(7),
        Constraint::Min(20),
        Constraint::Length(14),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(9),
        Constraint::Length(2),
    ];
    if medium {
        header.extend(["THR", "RUNTIME"]);
        widths.extend([Constraint::Length(4), Constraint::Length(8)]);
    }
    if wide {
        header.extend(["READ", "WRITE", "SERVICE/CGROUP"]);
        widths.extend([
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(24),
        ]);
    }
    let table = Table::new(rows, widths)
        .header(
            Row::new(header)
                .style(Style::default().add_modifier(Modifier::BOLD))
                .bottom_margin(1),
        )
        .block(Block::default().borders(Borders::TOP))
        .row_highlight_style(selection_style(app.capabilities))
        .highlight_symbol(if app.capabilities.unicode { "▸ " } else { "> " });
    let mut state = TableState::default().with_selected((!visible.is_empty()).then_some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_detail(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(process) = app.selected_process() else {
        frame.render_widget(Paragraph::new("No process selected"), area);
        return;
    };
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Length(5),
            Constraint::Min(6),
        ])
        .split(area);
    let identity = vec![
        Line::from(vec![
            Span::styled(
                format!("{} ({})", process.name, process.pid.0),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("  {}", process.state.label())),
        ]),
        Line::from(format!(
            "Parent {}  User {} (UID {})  Threads {}  Runtime {}",
            process
                .parent_pid
                .map_or_else(|| "-".to_owned(), |pid| pid.0.to_string()),
            process.user.display_name(),
            process.user.uid,
            process.threads,
            format_duration(process.runtime_seconds),
        )),
        Line::from(format!(
            "CPU {:.1}%  Memory {:.1}%  RSS {}  Virtual {}",
            process.cpu_percent,
            process.memory_percent,
            format_bytes(process.rss_bytes),
            format_bytes(process.virtual_memory_bytes),
        )),
        Line::from(format!(
            "Read {} (total {})  Write {} (total {})",
            format_rate(process.io.read_bytes_per_second),
            format_bytes(process.io.read_bytes),
            format_rate(process.io.write_bytes_per_second),
            format_bytes(process.io.write_bytes),
        )),
        Line::from(format!(
            "FDs {}  Children {}",
            process
                .file_descriptor_count
                .map_or_else(|| "unavailable".to_owned(), |count| count.to_string()),
            process.child_pids.len(),
        )),
    ];
    frame.render_widget(
        Paragraph::new(identity)
            .block(Block::default().borders(Borders::ALL).title(" Process ")),
        sections[0],
    );

    let charts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(sections[1]);
    let cpu_data = app.selected_cpu_history();
    frame.render_widget(
        Sparkline::default()
            .block(Block::default().borders(Borders::ALL).title(" CPU history "))
            .data(&cpu_data)
            .style(info_style(app.capabilities)),
        charts[0],
    );
    let memory_data = app.selected_memory_history();
    frame.render_widget(
        Sparkline::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" RSS history (KiB) "),
            )
            .data(&memory_data)
            .style(attention_style(app.capabilities)),
        charts[1],
    );

    let command = process.command_line.as_deref().unwrap_or("unavailable");
    let service = process
        .service
        .as_ref()
        .map(|service| service.name.as_str())
        .unwrap_or("-");
    let cgroup = process
        .cgroup
        .as_ref()
        .map(|cgroup| cgroup.path.as_str())
        .unwrap_or("-");
    let container = process
        .container
        .as_ref()
        .map(|container| container.id.as_str())
        .unwrap_or("-");
    let mut context = vec![
        Line::from(vec![Span::styled("Command: ", bold()), Span::raw(command)]),
        Line::from(vec![Span::styled("Executable: ", bold()), Span::raw(process.executable.as_deref().unwrap_or("-"))]),
        Line::from(format!("Service: {service}  Cgroup: {cgroup}")),
        Line::from(format!("Container: {container}")),
    ];
    if !process.unavailable_fields.is_empty() {
        context.push(Line::from(vec![
            Span::styled("Unavailable: ", attention_style(app.capabilities)),
            Span::raw(process.unavailable_fields.join(", ")),
        ]));
    }
    let findings: Vec<_> = app
        .snapshot
        .findings
        .iter()
        .filter(|finding| {
            finding.related_entities.iter().any(|entity| {
                matches!(entity, EntityId::Process { pid, start_ticks } if *pid == process.pid && *start_ticks == process.start_time_ticks)
            })
        })
        .collect();
    for finding in findings {
        context.push(Line::from(vec![
            Span::styled(
                format!("{}: ", finding.severity.label()),
                severity_style(finding.severity, app.capabilities),
            ),
            Span::raw(&finding.summary),
        ]));
    }
    frame.render_widget(
        Paragraph::new(context)
            .block(Block::default().borders(Borders::ALL).title(" Context and findings "))
            .wrap(Wrap { trim: false }),
        sections[2],
    );
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if let Some(error) = &app.error {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);
        frame.render_widget(
            Paragraph::new(format!(" Collection error: {error}"))
                .style(critical_style(app.capabilities)),
            rows[0],
        );
        frame.render_widget(Paragraph::new(footer_text(app)), rows[1]);
    } else {
        frame.render_widget(Paragraph::new(footer_text(app)), area);
    }
}

fn footer_text(app: &App) -> Line<'static> {
    let pause = if app.paused { "Space Resume" } else { "Space Pause" };
    Line::from(format!(
        " / Search   f Filter   s Sort   g Group   Enter Inspect   {pause}   r Refresh   ? Help   q Quit"
    ))
}

fn draw_input(frame: &mut Frame<'_>, area: Rect, app: &App, mode: InputMode) {
    let popup = centered_rect(80, 3, area);
    frame.render_widget(Clear, popup);
    let title = match mode {
        InputMode::Search => " Search name, command, PID, user, service or cgroup ",
        InputMode::Filter => " Filter: user: state: cpu:> mem:> name: service: ",
    };
    frame.render_widget(
        Paragraph::new(app.input_buffer.as_str())
            .block(Block::default().borders(Borders::ALL).title(title)),
        popup,
    );
    let cursor_x = popup.x + 1 + app.input_buffer.chars().count() as u16;
    frame.set_cursor_position((cursor_x.min(popup.right().saturating_sub(2)), popup.y + 1));
}

fn draw_help(frame: &mut Frame<'_>, area: Rect, capabilities: TerminalCapabilities) {
    let popup = centered_rect(76, 22, area);
    frame.render_widget(Clear, popup);
    let help = vec![
        Line::from("Move                 Up/Down or j/k"),
        Line::from("Inspect process      Enter"),
        Line::from("Back/close           Esc"),
        Line::from("Search               /"),
        Line::from("Filter expression    f"),
        Line::from("Choose sort          s"),
        Line::from("Cycle grouping       g"),
        Line::from("Next/previous        Tab / Shift+Tab"),
        Line::from("Pause/resume         Space"),
        Line::from("Refresh now          r"),
        Line::from("Help                  ?"),
        Line::from("Quit                  q or Ctrl+C"),
        Line::from(""),
        Line::from("Filter examples:"),
        Line::from("  user:postgres cpu:>5"),
        Line::from("  state:zombie"),
        Line::from("  service:sshd nginx"),
    ];
    frame.render_widget(
        Paragraph::new(help)
            .block(Block::default().borders(Borders::ALL).title(" Lens keys "))
            .style(info_style(capabilities)),
        popup,
    );
}

fn draw_sort(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup = centered_rect(44, 14, area);
    frame.render_widget(Clear, popup);
    let items: Vec<ListItem<'_>> = SortKey::ALL
        .iter()
        .map(|key| ListItem::new(key.label()))
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(
                    " Sort ({}) · d reverses ",
                    direction_symbol(app.sort_direction, app.capabilities)
                )),
        )
        .highlight_style(selection_style(app.capabilities))
        .highlight_symbol(if app.capabilities.unicode { "▸ " } else { "> " });
    let mut state = ListState::default().with_selected(Some(app.sort_selection));
    frame.render_stateful_widget(list, popup, &mut state);
}

fn draw_too_small(frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from("Dataplicity Lens"),
            Line::from("Terminal is too small."),
            Line::from(format!("Current: {}x{}  Required: 58x12", area.width, area.height)),
            Line::from("Resize the terminal or press q to quit."),
        ])
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(height.min(area.height)),
            Constraint::Fill(1),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

fn direction_symbol(direction: SortDirection, capabilities: TerminalCapabilities) -> &'static str {
    match (direction, capabilities.unicode) {
        (SortDirection::Ascending, true) => "↑",
        (SortDirection::Descending, true) => "↓",
        (SortDirection::Ascending, false) => "asc",
        (SortDirection::Descending, false) => "desc",
    }
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

fn info_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default().fg(if capabilities.true_color {
            Color::Rgb(80, 170, 255)
        } else {
            Color::Blue
        })
    } else {
        Style::default()
    }
}

fn attention_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn critical_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn selection_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default()
            .bg(if capabilities.true_color {
                Color::Rgb(32, 72, 104)
            } else {
                Color::DarkGray
            })
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::REVERSED)
    }
}

fn severity_style(severity: Severity, capabilities: TerminalCapabilities) -> Style {
    match severity {
        Severity::Information => info_style(capabilities),
        Severity::Attention => attention_style(capabilities),
        Severity::Critical => critical_style(capabilities),
    }
}

#[cfg(test)]
mod tests {
    use lens_core::{GroupMode, ProcessFilter, SortDirection, SortKey};
    use lens_model::Snapshot;
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::{ColorMode, TerminalCapabilities, UiOptions, app::App};

    #[test]
    fn narrow_screen_renders_without_panic() {
        let backend = TestBackend::new(60, 14);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let options = UiOptions {
            interval: std::time::Duration::from_secs(1),
            sort_key: SortKey::Cpu,
            sort_direction: SortDirection::Descending,
            group: GroupMode::None,
            filter: ProcessFilter::default(),
            limit: None,
            history_length: 10,
            color_mode: ColorMode::Never,
            ascii: true,
        };
        let capabilities = TerminalCapabilities::detect(ColorMode::Never, true);
        let mut app = App::new(Snapshot::empty("fixture"), options, capabilities);
        terminal.draw(|frame| draw(frame, &mut app)).expect("render");
    }
}
