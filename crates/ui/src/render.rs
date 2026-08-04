use lens_core::{SortDirection, SortKey};
use lens_model::{EntityId, Severity};
use lens_output::{format_bytes, format_duration, format_rate, truncate};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row,
        Sparkline, Table, TableState, Wrap,
    },
};

use crate::{
    TerminalCapabilities,
    app::{App, InputMode, View},
};

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    if area.width < 58 || area.height < 16 {
        draw_too_small(frame, area);
        return;
    }

    frame.render_widget(Block::default().style(canvas_style(app.capabilities)), area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Min(4),
            Constraint::Length(if app.error.is_some() { 3 } else { 2 }),
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
    let status = if app.paused { " PAUSED " } else { " LIVE " };
    let diamond = if app.capabilities.unicode {
        "  ◆  "
    } else {
        "  *  "
    };
    let separator = if app.capabilities.unicode {
        "   •   "
    } else {
        "   |   "
    };
    let status_style = if app.paused {
        attention_badge(app.capabilities)
    } else {
        success_badge(app.capabilities)
    };
    let title = Line::from(vec![
        Span::raw("  "),
        Span::styled("DATAPLICITY", brand_style(app.capabilities)),
        Span::styled(" / LENS", title_style(app.capabilities)),
        Span::styled(diamond, muted_style(app.capabilities)),
        Span::styled(&app.snapshot.host.hostname, info_style(app.capabilities)),
        Span::raw("  "),
        Span::styled(status, status_style),
    ]);
    let meta = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("refresh {:.1}s", app.interval().as_secs_f64()),
            muted_style(app.capabilities),
        ),
        Span::styled(separator, faint_style(app.capabilities)),
        Span::styled(
            format!("group {}", app.group.label()),
            muted_style(app.capabilities),
        ),
        Span::styled(separator, faint_style(app.capabilities)),
        Span::styled(
            format!(
                "sort {} {}",
                app.sort_key.label(),
                direction_symbol(app.sort_direction, app.capabilities)
            ),
            muted_style(app.capabilities),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(vec![title, meta]).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(border_style(app.capabilities)),
        ),
        area,
    );
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
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border_style(app.capabilities))
                .title(Line::from(vec![
                    Span::styled(" CPU  ", label_style(app.capabilities)),
                    Span::styled(
                        format!("{:>5.1}% ", app.snapshot.host.cpu_percent),
                        metric_style(app.capabilities),
                    ),
                ])),
        )
        .data(&cpu_data)
        .max(100)
        .style(info_style(app.capabilities));
    frame.render_widget(cpu, columns[0]);

    let memory = Sparkline::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border_style(app.capabilities))
                .title(Line::from(vec![
                    Span::styled(" MEMORY  ", label_style(app.capabilities)),
                    Span::styled(
                        format!("{:>5.1}% ", app.snapshot.host.memory.used_percent()),
                        attention_style(app.capabilities).add_modifier(Modifier::BOLD),
                    ),
                ])),
        )
        .data(&memory_data)
        .max(100)
        .style(attention_style(app.capabilities));
    frame.render_widget(memory, columns[1]);

    let counts = app.snapshot.host.process_counts;
    let summary = vec![
        Line::from(vec![
            Span::styled(" LOAD   ", label_style(app.capabilities)),
            Span::styled(
                format!(
                    "{:.2}  {:.2}  {:.2}",
                    app.snapshot.host.load.one,
                    app.snapshot.host.load.five,
                    app.snapshot.host.load.fifteen
                ),
                metric_style(app.capabilities),
            ),
        ]),
        Line::from(vec![
            Span::styled(" TASKS  ", label_style(app.capabilities)),
            Span::styled(counts.total.to_string(), title_style(app.capabilities)),
            Span::styled(
                format!("  {} running", counts.running),
                success_style(app.capabilities),
            ),
        ]),
        Line::from(vec![
            Span::styled(" ALERTS ", label_style(app.capabilities)),
            Span::styled(
                format!("{} findings", app.snapshot.findings.len()),
                if app.snapshot.findings.is_empty() {
                    success_style(app.capabilities)
                } else {
                    attention_style(app.capabilities)
                },
            ),
            Span::styled(
                format!("  {} zombie", counts.zombie),
                critical_style(app.capabilities),
            ),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(summary).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border_style(app.capabilities))
                .title(Span::styled(
                    " SYSTEM PULSE ",
                    label_style(app.capabilities),
                )),
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
            Cell::from(process.pid.0.to_string()).style(muted_style(app.capabilities)),
            Cell::from(truncate(&name, 30)).style(title_style(app.capabilities)),
            Cell::from(truncate(&process.user.display_name(), 14))
                .style(muted_style(app.capabilities)),
            Cell::from(format!("{:.1}", process.cpu_percent))
                .style(cpu_style(process.cpu_percent, app.capabilities)),
            Cell::from(format!("{:.1}", process.memory_percent))
                .style(memory_style(process.memory_percent, app.capabilities)),
            Cell::from(format_bytes(process.rss_bytes)).style(metric_style(app.capabilities)),
            Cell::from(process.state.short()).style(state_style(process.state, app.capabilities)),
        ];
        if medium {
            cells
                .push(Cell::from(process.threads.to_string()).style(muted_style(app.capabilities)));
            cells.push(
                Cell::from(format_duration(process.runtime_seconds))
                    .style(muted_style(app.capabilities)),
            );
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
        } else if item.index % 2 == 1 {
            alternate_row_style(app.capabilities)
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
                .style(table_header_style(app.capabilities))
                .bottom_margin(0),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border_style(app.capabilities))
                .title(Span::styled(
                    format!(" PROCESSES  {} visible ", visible.len()),
                    label_style(app.capabilities),
                )),
        )
        .row_highlight_style(selection_style(app.capabilities))
        .highlight_symbol(if app.capabilities.unicode {
            "▸ "
        } else {
            "> "
        });
    let mut state =
        TableState::default().with_selected((!visible.is_empty()).then_some(app.selected));
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
                format!("{} ", process.name),
                title_style(app.capabilities).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" PID {} ", process.pid.0),
                quiet_badge(app.capabilities),
            ),
            Span::raw("  "),
            Span::styled(
                process.state.label(),
                state_style(process.state, app.capabilities),
            ),
        ]),
        detail_line(
            "OWNER",
            format!(
                "{} (uid {})   parent {}   {} threads   {} runtime",
                process.user.display_name(),
                process.user.uid,
                process
                    .parent_pid
                    .map_or_else(|| "-".to_owned(), |pid| pid.0.to_string()),
                process.threads,
                format_duration(process.runtime_seconds),
            ),
            app.capabilities,
        ),
        Line::from(vec![
            Span::styled(" CPU     ", label_style(app.capabilities)),
            Span::styled(
                format!("{:.1}%", process.cpu_percent),
                cpu_style(process.cpu_percent, app.capabilities),
            ),
            Span::styled("     MEMORY  ", label_style(app.capabilities)),
            Span::styled(
                format!("{:.1}%", process.memory_percent),
                memory_style(process.memory_percent, app.capabilities),
            ),
            Span::styled("     RSS  ", label_style(app.capabilities)),
            Span::styled(
                format_bytes(process.rss_bytes),
                metric_style(app.capabilities),
            ),
            Span::styled("     VIRTUAL  ", label_style(app.capabilities)),
            Span::styled(
                format_bytes(process.virtual_memory_bytes),
                muted_style(app.capabilities),
            ),
        ]),
        detail_line(
            "I/O",
            format!(
                "read {} ({})   write {} ({})",
                format_rate(process.io.read_bytes_per_second),
                format_bytes(process.io.read_bytes),
                format_rate(process.io.write_bytes_per_second),
                format_bytes(process.io.write_bytes),
            ),
            app.capabilities,
        ),
        detail_line(
            "HANDLES",
            format!(
                "{} file descriptors   {} children",
                process
                    .file_descriptor_count
                    .map_or_else(|| "unavailable".to_owned(), |count| count.to_string()),
                process.child_pids.len(),
            ),
            app.capabilities,
        ),
    ];
    frame.render_widget(
        Paragraph::new(identity).block(panel(" PROCESS IDENTITY ", app.capabilities)),
        sections[0],
    );

    let charts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(sections[1]);
    let cpu_data = app.selected_cpu_history();
    frame.render_widget(
        Sparkline::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(border_style(app.capabilities))
                    .title(Span::styled(" CPU HISTORY ", label_style(app.capabilities))),
            )
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
                    .border_type(BorderType::Rounded)
                    .border_style(border_style(app.capabilities))
                    .title(Span::styled(
                        " RSS HISTORY  KiB ",
                        label_style(app.capabilities),
                    )),
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
        Line::from(vec![
            Span::styled(" COMMAND     ", label_style(app.capabilities)),
            Span::styled(command, title_style(app.capabilities)),
        ]),
        Line::from(vec![
            Span::styled(" EXECUTABLE  ", label_style(app.capabilities)),
            Span::styled(
                process.executable.as_deref().unwrap_or("-"),
                muted_style(app.capabilities),
            ),
        ]),
        detail_line(
            "SERVICE",
            format!("{service}   cgroup {cgroup}"),
            app.capabilities,
        ),
        detail_line("CONTAINER", container.to_owned(), app.capabilities),
    ];
    if !process.unavailable_fields.is_empty() {
        context.push(Line::from(vec![
            Span::styled(" UNAVAILABLE ", attention_badge(app.capabilities)),
            Span::raw("  "),
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
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(border_style(app.capabilities))
                    .title(Span::styled(
                        " CONTEXT & FINDINGS ",
                        label_style(app.capabilities),
                    )),
            )
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
                .style(critical_badge(app.capabilities)),
            rows[0],
        );
        frame.render_widget(
            Paragraph::new(footer_text(app)).block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(border_style(app.capabilities)),
            ),
            rows[1],
        );
    } else {
        frame.render_widget(
            Paragraph::new(footer_text(app)).block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(border_style(app.capabilities)),
            ),
            area,
        );
    }
}

fn footer_text(app: &App) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (key, action) in [
        ("/", "search"),
        ("f", "filter"),
        ("s", "sort"),
        ("g", "group"),
        ("↵", "inspect"),
        ("space", if app.paused { "resume" } else { "pause" }),
        ("r", "refresh"),
        ("?", "help"),
        ("q", "quit"),
    ] {
        spans.push(Span::styled(
            format!(" {key} "),
            key_style(app.capabilities),
        ));
        spans.push(Span::styled(
            format!(" {action}  "),
            muted_style(app.capabilities),
        ));
    }
    Line::from(spans)
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
            .style(title_style(app.capabilities))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(brand_style(app.capabilities))
                    .style(canvas_style(app.capabilities))
                    .title(Span::styled(title, label_style(app.capabilities))),
            ),
        popup,
    );
    let cursor_x = popup.x + 1 + app.input_buffer.chars().count() as u16;
    frame.set_cursor_position((cursor_x.min(popup.right().saturating_sub(2)), popup.y + 1));
}

fn draw_help(frame: &mut Frame<'_>, area: Rect, capabilities: TerminalCapabilities) {
    let popup = centered_rect(76, 22, area);
    frame.render_widget(Clear, popup);
    let help = vec![
        help_line("↑/↓  j/k", "Move through processes", capabilities),
        help_line("↵", "Inspect selected process", capabilities),
        help_line("esc", "Go back or close", capabilities),
        help_line("/", "Search everything", capabilities),
        help_line("f", "Filter expression", capabilities),
        help_line("s", "Choose sort order", capabilities),
        help_line("g", "Cycle grouping", capabilities),
        help_line("⇥ / ⇧⇥", "Next / previous", capabilities),
        help_line("space", "Pause or resume", capabilities),
        help_line("r", "Refresh now", capabilities),
        help_line("?", "Open this guide", capabilities),
        help_line("q / ctrl-c", "Quit safely", capabilities),
        Line::from(""),
        Line::from(Span::styled(" FILTER RECIPES", label_style(capabilities))),
        Line::from(Span::styled(
            "  user:postgres cpu:>5",
            info_style(capabilities),
        )),
        Line::from(Span::styled(
            "  state:zombie   service:sshd nginx",
            info_style(capabilities),
        )),
    ];
    frame.render_widget(
        Paragraph::new(help).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(brand_style(capabilities))
                .style(canvas_style(capabilities))
                .title(Span::styled(" COMMAND PALETTE ", brand_style(capabilities))),
        ),
        popup,
    );
}

fn draw_sort(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup = centered_rect(44, 14, area);
    frame.render_widget(Clear, popup);
    let items: Vec<ListItem<'_>> = SortKey::ALL
        .iter()
        .map(|key| {
            ListItem::new(Line::from(vec![
                Span::styled("  ", muted_style(app.capabilities)),
                Span::styled(key.label(), title_style(app.capabilities)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(brand_style(app.capabilities))
                .style(canvas_style(app.capabilities))
                .title(Line::from(vec![
                    Span::styled(" SORT BY  ", brand_style(app.capabilities)),
                    Span::styled(
                        direction_symbol(app.sort_direction, app.capabilities),
                        metric_style(app.capabilities),
                    ),
                    Span::styled("   d reverses ", muted_style(app.capabilities)),
                ])),
        )
        .highlight_style(selection_style(app.capabilities))
        .highlight_symbol(if app.capabilities.unicode {
            "▸ "
        } else {
            "> "
        });
    let mut state = ListState::default().with_selected(Some(app.sort_selection));
    frame.render_stateful_widget(list, popup, &mut state);
}

fn draw_too_small(frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "DATAPLICITY / LENS",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("Terminal is too small."),
            Line::from(format!(
                "Current: {}x{}  Required: 58x16",
                area.width, area.height
            )),
            Line::from("Resize the terminal or press q to quit."),
        ])
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded),
        ),
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

fn rgb(capabilities: TerminalCapabilities, true_color: (u8, u8, u8), fallback: Color) -> Color {
    if capabilities.true_color {
        Color::Rgb(true_color.0, true_color.1, true_color.2)
    } else {
        fallback
    }
}

fn canvas_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default()
            .fg(rgb(capabilities, (207, 216, 230), Color::White))
            .bg(rgb(capabilities, (10, 14, 23), Color::Black))
    } else {
        Style::default()
    }
}

fn brand_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default()
            .fg(rgb(capabilities, (190, 125, 255), Color::Magenta))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn title_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default().fg(rgb(capabilities, (238, 243, 252), Color::White))
    } else {
        Style::default()
    }
}

fn label_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default()
            .fg(rgb(capabilities, (139, 155, 180), Color::Gray))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn muted_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default().fg(rgb(capabilities, (125, 140, 165), Color::Gray))
    } else {
        Style::default()
    }
}

fn faint_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default().fg(rgb(capabilities, (52, 64, 84), Color::DarkGray))
    } else {
        Style::default()
    }
}

fn border_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default().fg(rgb(capabilities, (48, 62, 84), Color::DarkGray))
    } else {
        Style::default()
    }
}

fn metric_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default()
            .fg(rgb(capabilities, (91, 215, 255), Color::Cyan))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn info_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default().fg(if capabilities.true_color {
            Color::Rgb(91, 215, 255)
        } else {
            Color::Blue
        })
    } else {
        Style::default()
    }
}

fn attention_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default().fg(rgb(capabilities, (255, 190, 92), Color::Yellow))
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn critical_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default()
            .fg(rgb(capabilities, (255, 105, 125), Color::Red))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn selection_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default()
            .bg(if capabilities.true_color {
                Color::Rgb(70, 49, 104)
            } else {
                Color::DarkGray
            })
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::REVERSED)
    }
}

fn success_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default().fg(rgb(capabilities, (88, 224, 166), Color::Green))
    } else {
        Style::default()
    }
}

fn success_badge(capabilities: TerminalCapabilities) -> Style {
    badge(
        capabilities,
        (17, 62, 50),
        (112, 239, 188),
        Color::Green,
        Color::White,
    )
}

fn attention_badge(capabilities: TerminalCapabilities) -> Style {
    badge(
        capabilities,
        (81, 52, 20),
        (255, 201, 112),
        Color::Yellow,
        Color::White,
    )
}

fn critical_badge(capabilities: TerminalCapabilities) -> Style {
    badge(
        capabilities,
        (92, 32, 45),
        (255, 166, 178),
        Color::Red,
        Color::White,
    )
}

fn quiet_badge(capabilities: TerminalCapabilities) -> Style {
    badge(
        capabilities,
        (31, 42, 61),
        (168, 183, 207),
        Color::DarkGray,
        Color::White,
    )
}

fn badge(
    capabilities: TerminalCapabilities,
    background: (u8, u8, u8),
    foreground: (u8, u8, u8),
    fallback_background: Color,
    fallback_foreground: Color,
) -> Style {
    if capabilities.color {
        Style::default()
            .bg(rgb(capabilities, background, fallback_background))
            .fg(rgb(capabilities, foreground, fallback_foreground))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    }
}

fn alternate_row_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default().bg(rgb(capabilities, (14, 20, 31), Color::Black))
    } else {
        Style::default()
    }
}

fn table_header_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default()
            .fg(rgb(capabilities, (158, 176, 204), Color::Gray))
            .bg(rgb(capabilities, (20, 28, 42), Color::Black))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    }
}

fn key_style(capabilities: TerminalCapabilities) -> Style {
    badge(
        capabilities,
        (31, 42, 61),
        (190, 125, 255),
        Color::DarkGray,
        Color::White,
    )
}

fn cpu_style(value: f64, capabilities: TerminalCapabilities) -> Style {
    if value >= 80.0 {
        critical_style(capabilities)
    } else if value >= 40.0 {
        attention_style(capabilities)
    } else {
        metric_style(capabilities)
    }
}

fn memory_style(value: f64, capabilities: TerminalCapabilities) -> Style {
    if value >= 80.0 {
        critical_style(capabilities)
    } else if value >= 50.0 {
        attention_style(capabilities)
    } else {
        info_style(capabilities)
    }
}

fn state_style(state: lens_model::ProcessState, capabilities: TerminalCapabilities) -> Style {
    match state {
        lens_model::ProcessState::Running => success_style(capabilities),
        lens_model::ProcessState::Zombie
        | lens_model::ProcessState::Dead
        | lens_model::ProcessState::DiskSleep => critical_style(capabilities),
        lens_model::ProcessState::Stopped | lens_model::ProcessState::TracingStop => {
            attention_style(capabilities)
        }
        _ => muted_style(capabilities),
    }
}

fn panel(title: &'static str, capabilities: TerminalCapabilities) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style(capabilities))
        .title(Span::styled(title, label_style(capabilities)))
}

fn detail_line(
    label: &'static str,
    value: String,
    capabilities: TerminalCapabilities,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {label:<10}"), label_style(capabilities)),
        Span::styled(value, muted_style(capabilities)),
    ])
}

fn help_line(
    key: &'static str,
    action: &'static str,
    capabilities: TerminalCapabilities,
) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!(" {key:^11} "), key_style(capabilities)),
        Span::styled(format!("  {action}"), title_style(capabilities)),
    ])
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
        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("render");
    }

    #[test]
    fn full_colour_dashboard_renders_without_panic() {
        let backend = TestBackend::new(140, 32);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let options = UiOptions {
            interval: std::time::Duration::from_secs(1),
            sort_key: SortKey::Cpu,
            sort_direction: SortDirection::Descending,
            group: GroupMode::None,
            filter: ProcessFilter::default(),
            limit: None,
            history_length: 10,
            color_mode: ColorMode::Always,
            ascii: false,
        };
        let capabilities = TerminalCapabilities {
            color: true,
            true_color: true,
            unicode: true,
        };
        let mut app = App::new(
            Snapshot::empty("production-gateway-04"),
            options,
            capabilities,
        );
        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("render full-colour dashboard");
    }
}
