use lens_core::{SortDirection, SortKey};
use lens_model::{EntityId, Severity};
use lens_output::{format_bytes, format_duration, format_rate, truncate};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::{bar, border},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row,
        Sparkline, Table, TableState, Wrap,
    },
};
use time::{OffsetDateTime, macros::format_description};

use crate::{
    TerminalCapabilities,
    app::{App, InputMode, ProcessActionStage, ProcessSignal, View},
};

/// Box drawing for terminals without Unicode support, notably a Linux console that is not running
/// in UTF-8 mode: it shows Unicode line drawing as mojibake instead.
const ASCII_BORDER: border::Set<'static> = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

const ASCII_BARS: bar::Set<'static> = bar::Set {
    full: "#",
    seven_eighths: "%",
    three_quarters: "*",
    five_eighths: "+",
    half: "=",
    three_eighths: "-",
    one_quarter: ":",
    one_eighth: ".",
    empty: " ",
};

fn frame_border(capabilities: TerminalCapabilities) -> border::Set<'static> {
    if capabilities.unicode {
        BorderType::Rounded.to_border_set()
    } else {
        ASCII_BORDER
    }
}

fn spark_bars(capabilities: TerminalCapabilities) -> bar::Set<'static> {
    if capabilities.unicode {
        bar::NINE_LEVELS
    } else {
        ASCII_BARS
    }
}

fn enter_key(capabilities: TerminalCapabilities) -> &'static str {
    if capabilities.unicode { "↵" } else { "Ent" }
}

fn separator(capabilities: TerminalCapabilities) -> &'static str {
    if capabilities.unicode { "·" } else { "-" }
}

fn ellipsis(capabilities: TerminalCapabilities) -> &'static str {
    if capabilities.unicode { "…" } else { "..." }
}

/// Replace any remaining non-ASCII cell in a rendered frame.
///
/// Frames carry text Lens does not author — process names, command lines, log messages — and a
/// terminal without Unicode support shows those bytes as mojibake, which also breaks column
/// alignment. Substituting one character per cell keeps the layout ratatui computed intact.
pub fn enforce_ascii(buffer: &mut Buffer) {
    for cell in &mut buffer.content {
        let symbol = cell.symbol();
        if symbol.is_ascii() {
            continue;
        }
        let replacement = match symbol.chars().next() {
            Some('·' | '─' | '━' | '–' | '—') => "-",
            Some('•' | '●' | '◆' | '◇') => "*",
            Some('…') => ".",
            Some('×') => "x",
            Some('→' | '▸' | '▶' | '›' | '⇒') => ">",
            Some('←') => "<",
            Some('↑') => "^",
            Some('↓') => "v",
            Some('↵' | '⏎' | '⇥') => "|",
            Some('│' | '┃') => "|",
            Some('╭' | '╮' | '╰' | '╯' | '├' | '┤' | '┌' | '┐' | '└' | '┘' | '┼') => {
                "+"
            }
            Some('▁'..='█') => "#",
            Some('‘' | '’') => "'",
            Some('“' | '”') => "\"",
            _ => "?",
        };
        cell.set_symbol(replacement);
    }
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    if area.width < 36 || area.height < 10 {
        draw_too_small(frame, area, app.capabilities);
        return;
    }

    frame.render_widget(Block::default().style(canvas_style(app.capabilities)), area);

    let show_summary = area.height >= 17;
    let summary_height = if !show_summary {
        0
    } else if area.width < 78 {
        4
    } else {
        5
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(summary_height),
            Constraint::Min(4),
            Constraint::Length(if app.error.is_some() { 3 } else { 2 }),
        ])
        .split(area);
    draw_header(frame, rows[0], app);
    if show_summary {
        draw_summary(frame, rows[1], app);
    }
    match app.view {
        View::List => draw_processes(frame, rows[2], app),
        View::Detail => draw_detail(frame, rows[2], app),
    }
    draw_footer(frame, rows[3], app);

    if app.process_action.is_some() {
        draw_process_action(frame, area, app);
    } else if app.diagnostic_open {
        draw_diagnostic(frame, area, app);
    } else if app.show_help {
        draw_help(frame, area, app.capabilities);
    } else if app.show_sort {
        draw_sort(frame, area, app);
    } else if let Some(mode) = app.input_mode {
        draw_input(frame, area, app, mode);
    }
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    // Keep the badge width fixed so starting and completing a sample never shifts the title row.
    let status = if app.paused {
        " PAUSED   "
    } else if app.collecting() {
        " UPDATING "
    } else {
        " LIVE     "
    };
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
    let status_style = if app.paused || app.collecting() {
        attention_badge(app.capabilities)
    } else {
        success_badge(app.capabilities)
    };
    let title = if area.width < 64 {
        Line::from(vec![
            Span::raw(" "),
            Span::styled("LENS", brand_style(app.capabilities)),
            Span::raw("  "),
            Span::styled(
                truncate(
                    &app.snapshot.host.hostname,
                    area.width.saturating_sub(20) as usize,
                ),
                info_style(app.capabilities),
            ),
            Span::raw(" "),
            Span::styled(status, status_style),
        ])
    } else {
        Line::from(vec![
            Span::raw("  "),
            Span::styled("DATAPLICITY", brand_style(app.capabilities)),
            Span::styled(" / LENS", title_style(app.capabilities)),
            Span::styled(diamond, muted_style(app.capabilities)),
            Span::styled(&app.snapshot.host.hostname, info_style(app.capabilities)),
            Span::raw("  "),
            Span::styled(status, status_style),
        ])
    };
    let refresh = format!("refresh {:.1}s", app.interval().as_secs_f64());
    let mut meta = vec![
        Span::raw("  "),
        Span::styled(refresh, muted_style(app.capabilities)),
    ];
    if area.width >= 64 {
        meta.extend([
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
    }
    if area.width >= 88 {
        meta.extend([
            Span::styled(separator, faint_style(app.capabilities)),
            Span::styled(
                format!("group {}", app.group.label()),
                muted_style(app.capabilities),
            ),
        ]);
    }
    if area.width >= 72 {
        meta.extend([
            Span::styled(separator, faint_style(app.capabilities)),
            Span::styled(local_clock(), muted_style(app.capabilities)),
        ]);
    }
    let meta = Line::from(meta);
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
    if area.width < 78 {
        let counts = app.snapshot.host.process_counts;
        let compact = vec![
            Line::from(vec![
                Span::styled(" CPU ", label_style(app.capabilities)),
                Span::styled(
                    format!("{:.1}%", app.snapshot.host.cpu_percent),
                    metric_style(app.capabilities),
                ),
                Span::styled("   MEM ", label_style(app.capabilities)),
                Span::styled(
                    format!("{:.1}%", app.snapshot.host.memory.used_percent()),
                    attention_style(app.capabilities),
                ),
                Span::styled("   LOAD ", label_style(app.capabilities)),
                Span::styled(
                    format!("{:.2}", app.snapshot.host.load.one),
                    metric_style(app.capabilities),
                ),
            ]),
            Line::from(vec![
                Span::styled(" TASKS ", label_style(app.capabilities)),
                Span::styled(counts.total.to_string(), title_style(app.capabilities)),
                Span::styled(
                    format!("   {} running", counts.running),
                    success_style(app.capabilities),
                ),
                Span::styled(
                    format!("   {} alerts", app.snapshot.findings.len()),
                    attention_style(app.capabilities),
                ),
            ]),
        ];
        frame.render_widget(
            Paragraph::new(compact).block(panel(" SYSTEM ", app.capabilities)),
            area,
        );
        return;
    }
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
                .border_set(frame_border(app.capabilities))
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
        .bar_set(spark_bars(app.capabilities))
        .style(info_style(app.capabilities));
    frame.render_widget(cpu, columns[0]);

    let memory = Sparkline::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_set(frame_border(app.capabilities))
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
        .bar_set(spark_bars(app.capabilities))
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
                .border_set(frame_border(app.capabilities))
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
    let compact = area.width < 58;
    let narrow = area.width < 78;
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
            Cell::from(truncate(&name, area.width.saturating_sub(24) as usize))
                .style(title_style(app.capabilities)),
        ];
        if !compact && !narrow {
            cells.push(
                Cell::from(truncate(&process.user.display_name(), 14))
                    .style(muted_style(app.capabilities)),
            );
        }
        cells.push(
            Cell::from(format!("{:.1}", process.cpu_percent))
                .style(cpu_style(process.cpu_percent, app.capabilities)),
        );
        if !compact {
            cells.push(
                Cell::from(format!("{:.1}", process.memory_percent))
                    .style(memory_style(process.memory_percent, app.capabilities)),
            );
        }
        if !compact && !narrow {
            cells.push(
                Cell::from(format_bytes(process.rss_bytes)).style(metric_style(app.capabilities)),
            );
        }
        cells.push(
            Cell::from(process.state.short()).style(state_style(process.state, app.capabilities)),
        );
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

    let mut header = vec!["PID", "PROCESS"];
    let mut widths = vec![Constraint::Length(7), Constraint::Min(12)];
    if !compact && !narrow {
        header.push("USER");
        widths.push(Constraint::Length(14));
    }
    header.push("CPU%");
    widths.push(Constraint::Length(6));
    if !compact {
        header.push("MEM%");
        widths.push(Constraint::Length(6));
    }
    if !compact && !narrow {
        header.push("RSS");
        widths.push(Constraint::Length(9));
    }
    header.push("ST");
    widths.push(Constraint::Length(2));
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
                .border_set(frame_border(app.capabilities))
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
    let capacity = area.height.saturating_sub(3) as usize;
    let offset = viewport_start(app.selected, visible.len(), capacity);
    let mut state = TableState::default()
        .with_offset(offset)
        .with_selected((!visible.is_empty()).then_some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn viewport_start(selected: usize, length: usize, capacity: usize) -> usize {
    if capacity == 0 || length <= capacity {
        0
    } else {
        selected
            .saturating_sub(capacity / 2)
            .min(length.saturating_sub(capacity))
    }
}

fn draw_detail(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(process) = app.selected_process() else {
        let message = app.inspected_process_identity().map_or_else(
            || "No process selected".to_owned(),
            |(pid, _)| {
                format!(
                    "Process PID {} is no longer running.\n\nPress Esc to return to the process list.",
                    pid.0
                )
            },
        );
        frame.render_widget(
            Paragraph::new(message)
                .block(panel(" PROCESS EXITED ", app.capabilities))
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    };
    let show_charts = area.height >= 16;
    let identity_height = if area.width < 72 { 6 } else { 7 };
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(identity_height),
            Constraint::Length(if show_charts { 5 } else { 0 }),
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

    if show_charts {
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
                        .border_set(frame_border(app.capabilities))
                        .border_style(border_style(app.capabilities))
                        .title(Span::styled(" CPU HISTORY ", label_style(app.capabilities))),
                )
                .data(&cpu_data)
                .bar_set(spark_bars(app.capabilities))
                .style(info_style(app.capabilities)),
            charts[0],
        );
        let memory_data = app.selected_memory_history();
        frame.render_widget(
            Sparkline::default()
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_set(frame_border(app.capabilities))
                        .border_style(border_style(app.capabilities))
                        .title(Span::styled(
                            " RSS HISTORY  KiB ",
                            label_style(app.capabilities),
                        )),
                )
                .data(&memory_data)
                .bar_set(spark_bars(app.capabilities))
                .style(attention_style(app.capabilities)),
            charts[1],
        );
    }

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
                    .border_set(frame_border(app.capabilities))
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
            Paragraph::new(footer_text(app, area.width)).block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(border_style(app.capabilities)),
            ),
            rows[1],
        );
    } else {
        frame.render_widget(
            Paragraph::new(footer_text(app, area.width)).block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(border_style(app.capabilities)),
            ),
            area,
        );
    }
}

fn footer_text(app: &App, width: u16) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    let mut actions = vec![
        (enter_key(app.capabilities), "inspect"),
        ("a", "action"),
        ("r", "refresh"),
        ("q", "quit"),
    ];
    if width >= 58 {
        actions.splice(0..0, [("/", "search"), ("f", "filter")]);
    }
    if width >= 88 {
        actions.splice(2..2, [("s", "sort"), ("g", "group")]);
        actions.push(("space", if app.paused { "resume" } else { "pause" }));
        actions.push(("!", "shell"));
        actions.push(("?", "help"));
    }
    for (key, action) in actions {
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
                    .border_set(frame_border(app.capabilities))
                    .border_style(brand_style(app.capabilities))
                    .style(canvas_style(app.capabilities))
                    .title(Span::styled(title, label_style(app.capabilities))),
            ),
        popup,
    );
    let cursor_x = popup.x + 1 + app.input_buffer.chars().count() as u16;
    frame.set_cursor_position((cursor_x.min(popup.right().saturating_sub(2)), popup.y + 1));
}

fn draw_process_action(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(dialog) = app.process_action.as_ref() else {
        return;
    };
    let popup = centered_rect(62, 13, area);
    frame.render_widget(Clear, popup);
    let signal = ProcessSignal::ALL[dialog.selection];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(frame_border(app.capabilities))
        .border_style(brand_style(app.capabilities))
        .style(canvas_style(app.capabilities))
        .title(Span::styled(
            " PROCESS ACTION ",
            brand_style(app.capabilities),
        ));

    match dialog.stage {
        ProcessActionStage::Choose => {
            let items: Vec<ListItem<'_>> = ProcessSignal::ALL
                .iter()
                .map(|signal| ListItem::new(signal.label()))
                .collect();
            let list = List::new(items)
                .block(block.title_bottom(Span::styled(
                    format!(
                        " {} {sep} PID {} {sep} Enter review {sep} Esc cancel ",
                        dialog.process,
                        dialog.pid,
                        sep = separator(app.capabilities)
                    ),
                    muted_style(app.capabilities),
                )))
                .highlight_style(selection_style(app.capabilities))
                .highlight_symbol(if app.capabilities.unicode {
                    "▸ "
                } else {
                    "> "
                });
            let mut state = ListState::default().with_selected(Some(dialog.selection));
            frame.render_stateful_widget(list, popup, &mut state);
        }
        ProcessActionStage::Confirm => {
            let warning = if signal == ProcessSignal::Kill {
                "KILL cannot be handled or cleaned up by the process."
            } else {
                "Lens will re-check the process identity before sending the signal."
            };
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        format!("Send {} to {}?", signal.short_name(), dialog.process),
                        title_style(app.capabilities).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(format!(
                        "PID {} {} start identity {}",
                        dialog.pid,
                        separator(app.capabilities),
                        dialog.start_time_ticks
                    )),
                    Line::from(""),
                    Line::from(Span::styled(warning, attention_style(app.capabilities))),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(" y ", key_style(app.capabilities)),
                        Span::raw(" confirm   "),
                        Span::styled(" Esc ", key_style(app.capabilities)),
                        Span::raw(" back"),
                    ]),
                ])
                .alignment(Alignment::Center)
                .block(block),
                popup,
            );
        }
        ProcessActionStage::Running => {
            frame.render_widget(
                Paragraph::new(format!(
                    "\nSending {} to {} (PID {}){}\n\nWaiting for operating-system verification.",
                    signal.short_name(),
                    dialog.process,
                    dialog.pid,
                    ellipsis(app.capabilities)
                ))
                .alignment(Alignment::Center)
                .block(block),
                popup,
            );
        }
        ProcessActionStage::Result => {
            frame.render_widget(
                Paragraph::new(format!("\n{}\n\nEnter or Esc to close", dialog.result))
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: false })
                    .block(block),
                popup,
            );
        }
    }
}

fn draw_diagnostic(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let panel = if area.width >= 120 && area.height >= 20 {
        Rect::new(
            area.x + area.width * 55 / 100,
            area.y + 2,
            area.width * 45 / 100 - 1,
            area.height.saturating_sub(4),
        )
    } else {
        Rect::new(
            area.x + 1,
            area.y + 1,
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        )
    };
    frame.render_widget(Clear, panel);
    let status = if app.diagnostic_running {
        format!(
            " Running command{} {} Esc close ",
            ellipsis(app.capabilities),
            separator(app.capabilities)
        )
    } else {
        format!(
            " Results appear here {} Esc close ",
            separator(app.capabilities)
        )
    };
    let inner_height = panel.height.saturating_sub(4) as usize;
    let output = if app.diagnostic_output.is_empty() {
        [
            "Run a command without leaving this view.",
            "Lens keeps updating behind this panel.",
            "",
            "Try one of these:",
            "  uptime",
            "  df -h",
            "  ps aux | head",
        ]
        .join("\n")
    } else {
        let start = app.diagnostic_output.len().saturating_sub(inner_height);
        app.diagnostic_output[start..].join("\n")
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(panel);
    frame.render_widget(
        Paragraph::new(output)
            .style(muted_style(app.capabilities))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_set(frame_border(app.capabilities))
                    .border_style(brand_style(app.capabilities))
                    .style(canvas_style(app.capabilities))
                    .title(Span::styled(
                        " COMMAND OUTPUT ",
                        label_style(app.capabilities),
                    ))
                    .title_bottom(Span::styled(status, muted_style(app.capabilities))),
            ),
        rows[0],
    );
    let prompt = if app.diagnostic_running {
        format!("Running{}", ellipsis(app.capabilities))
    } else {
        format!("$ {}", app.diagnostic_input)
    };
    frame.render_widget(
        Paragraph::new(prompt)
            .style(title_style(app.capabilities))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_set(frame_border(app.capabilities))
                    .border_style(border_style(app.capabilities))
                    .style(canvas_style(app.capabilities))
                    .title(Span::styled(" COMMAND ", label_style(app.capabilities)))
                    .title_bottom(Span::styled(
                        " Enter to run ",
                        muted_style(app.capabilities),
                    )),
            ),
        rows[1],
    );
    if !app.diagnostic_running {
        let cursor_x = rows[1].x + 3 + app.diagnostic_input.chars().count() as u16;
        frame.set_cursor_position((
            cursor_x.min(rows[1].right().saturating_sub(2)),
            rows[1].y + 1,
        ));
    }
}

fn local_clock() -> String {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    now.format(format_description!("[hour]:[minute]:[second]"))
        .unwrap_or_else(|_| "--:--:--".to_owned())
}

fn draw_help(frame: &mut Frame<'_>, area: Rect, capabilities: TerminalCapabilities) {
    let popup = centered_rect(76, 22, area);
    frame.render_widget(Clear, popup);
    let help = vec![
        help_line(
            if capabilities.unicode {
                "↑/↓  j/k"
            } else {
                "up/dn j/k"
            },
            "Move through processes",
            capabilities,
        ),
        help_line(
            enter_key(capabilities),
            "Inspect selected process",
            capabilities,
        ),
        help_line("a", "Act on selected process", capabilities),
        help_line("esc", "Go back or close", capabilities),
        help_line("/", "Search everything", capabilities),
        help_line("f", "Filter expression", capabilities),
        help_line("s", "Choose sort order", capabilities),
        help_line("g", "Cycle grouping", capabilities),
        help_line(
            if capabilities.unicode {
                "⇥ / ⇧⇥"
            } else {
                "Tab / S-Tab"
            },
            "Next / previous",
            capabilities,
        ),
        help_line("space", "Pause or resume", capabilities),
        help_line("r", "Refresh now", capabilities),
        help_line("!", "Open diagnostic shell", capabilities),
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
                .border_set(frame_border(capabilities))
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
                .border_set(frame_border(app.capabilities))
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

fn draw_too_small(frame: &mut Frame<'_>, area: Rect, capabilities: TerminalCapabilities) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "DATAPLICITY / LENS",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("Terminal is too small."),
            Line::from(format!(
                "Current: {}x{}  Required: 36x10",
                area.width, area.height
            )),
            Line::from("Resize the terminal or press q to quit."),
        ])
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_set(frame_border(capabilities)),
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

fn rgb(_capabilities: TerminalCapabilities, true_color: (u8, u8, u8), _fallback: Color) -> Color {
    // Browser, serial and embedded terminals commonly support 24-bit colour without advertising
    // COLORTERM. Their configurable ANSI palettes are also too inconsistent for accessibility:
    // "dark gray" is frequently rendered almost white. Emit the deliberate Lens RGB palette
    // whenever colour is enabled so light-background contrast does not depend on terminal metadata.
    Color::Rgb(true_color.0, true_color.1, true_color.2)
}

fn theme_rgb(
    capabilities: TerminalCapabilities,
    dark: (u8, u8, u8),
    light: (u8, u8, u8),
    fallback: Color,
) -> Color {
    let fallback = if capabilities.light_background {
        match fallback {
            Color::White => Color::Black,
            Color::Gray => Color::DarkGray,
            Color::Cyan => Color::Blue,
            Color::Yellow => Color::DarkGray,
            Color::Black => Color::Gray,
            other => other,
        }
    } else {
        fallback
    };
    rgb(
        capabilities,
        if capabilities.light_background {
            light
        } else {
            dark
        },
        fallback,
    )
}

fn canvas_style(capabilities: TerminalCapabilities) -> Style {
    let _ = capabilities;
    // Keep both foreground and background under terminal control. Browser and serial terminals do
    // not consistently expose their background metadata, but their configured default foreground
    // is already chosen for legibility.
    Style::default()
}

fn brand_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default()
            .fg(theme_rgb(
                capabilities,
                (143, 91, 215),
                (105, 40, 160),
                Color::Magenta,
            ))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn title_style(capabilities: TerminalCapabilities) -> Style {
    let _ = capabilities;
    Style::default().add_modifier(Modifier::BOLD)
}

fn label_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default()
            .fg(theme_rgb(
                capabilities,
                (105, 116, 132),
                (70, 85, 105),
                Color::Gray,
            ))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn muted_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default().fg(theme_rgb(
            capabilities,
            (105, 116, 132),
            (78, 94, 112),
            Color::Gray,
        ))
    } else {
        Style::default()
    }
}

fn faint_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default().fg(theme_rgb(
            capabilities,
            (52, 64, 84),
            (122, 136, 151),
            Color::DarkGray,
        ))
    } else {
        Style::default()
    }
}

fn border_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default().fg(theme_rgb(
            capabilities,
            (48, 62, 84),
            (148, 160, 174),
            Color::DarkGray,
        ))
    } else {
        Style::default()
    }
}

fn metric_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default()
            .fg(theme_rgb(
                capabilities,
                (0, 126, 163),
                (0, 103, 148),
                Color::Cyan,
            ))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn info_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default().fg(theme_rgb(
            capabilities,
            (0, 126, 163),
            (0, 103, 148),
            Color::Blue,
        ))
    } else {
        Style::default()
    }
}

fn attention_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default().fg(theme_rgb(
            capabilities,
            (166, 95, 0),
            (145, 79, 0),
            Color::Yellow,
        ))
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn critical_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default()
            .fg(theme_rgb(
                capabilities,
                (199, 51, 80),
                (185, 30, 55),
                Color::Red,
            ))
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
        Style::default().fg(theme_rgb(
            capabilities,
            (0, 137, 94),
            (0, 110, 79),
            Color::Green,
        ))
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
    let _ = capabilities;
    Style::default()
}

fn table_header_style(capabilities: TerminalCapabilities) -> Style {
    if capabilities.color {
        Style::default()
            .fg(theme_rgb(
                capabilities,
                (105, 116, 132),
                (56, 72, 92),
                Color::Gray,
            ))
            .bg(theme_rgb(
                capabilities,
                (20, 28, 42),
                (231, 236, 240),
                Color::Black,
            ))
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
        .border_set(frame_border(capabilities))
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

    fn rendered_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn narrow_screen_renders_without_panic() {
        let backend = TestBackend::new(36, 10);
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
            theme_mode: crate::ThemeMode::Auto,
            ascii: true,
        };
        let capabilities = TerminalCapabilities::detect(ColorMode::Never, true);
        let mut app = App::new(Snapshot::empty("fixture"), options, capabilities);
        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("render");
        let rendered = rendered_text(&terminal);
        assert!(rendered.contains("PROCESS"));
        assert!(!rendered.contains("USER"));
        assert!(!rendered.contains("MEMORY"));
    }

    #[test]
    fn medium_screen_reflows_without_panic() {
        let backend = TestBackend::new(76, 18);
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
            theme_mode: crate::ThemeMode::Auto,
            ascii: true,
        };
        let capabilities = TerminalCapabilities::detect(ColorMode::Never, true);
        let mut app = App::new(Snapshot::empty("fixture"), options, capabilities);
        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("render");
    }

    #[test]
    fn process_viewport_centres_selection_away_from_the_edges() {
        assert_eq!(viewport_start(50, 100, 20), 40);
        assert_eq!(viewport_start(3, 100, 20), 0);
        assert_eq!(viewport_start(98, 100, 20), 80);
        assert_eq!(viewport_start(3, 10, 20), 0);
    }

    #[test]
    fn full_colour_dashboard_renders_without_panic() {
        let backend = TestBackend::new(180, 50);
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
            theme_mode: crate::ThemeMode::Dark,
            ascii: false,
        };
        let capabilities = TerminalCapabilities {
            color: true,
            true_color: true,
            unicode: true,
            light_background: false,
        };
        let mut app = App::new(
            Snapshot::empty("production-gateway-04"),
            options,
            capabilities,
        );
        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("render full-colour dashboard");
        let rendered = rendered_text(&terminal);
        assert!(rendered.contains("SERVICE/CGROUP"));
        assert!(rendered.contains("MEMORY"));
    }

    #[test]
    fn light_theme_uses_dark_primary_text() {
        let capabilities = TerminalCapabilities {
            color: true,
            true_color: true,
            unicode: true,
            light_background: true,
        };

        assert_eq!(canvas_style(capabilities).fg, None);
        assert_eq!(title_style(capabilities).fg, None);
        assert_eq!(muted_style(capabilities).fg, Some(Color::Rgb(78, 94, 112)));
    }

    #[test]
    fn automatic_palette_stays_legible_when_background_metadata_is_missing() {
        let capabilities = TerminalCapabilities {
            color: true,
            true_color: true,
            unicode: true,
            light_background: false,
        };

        assert_eq!(canvas_style(capabilities).fg, None);
        assert_eq!(title_style(capabilities).fg, None);
        assert_eq!(metric_style(capabilities).fg, Some(Color::Rgb(0, 126, 163)));
        assert_eq!(
            muted_style(capabilities).fg,
            Some(Color::Rgb(105, 116, 132))
        );
    }

    #[test]
    fn browser_terminals_receive_the_contrast_safe_rgb_palette() {
        let capabilities = TerminalCapabilities {
            color: true,
            true_color: false,
            unicode: true,
            light_background: true,
        };

        assert_eq!(label_style(capabilities).fg, Some(Color::Rgb(70, 85, 105)));
        assert_eq!(muted_style(capabilities).fg, Some(Color::Rgb(78, 94, 112)));
        assert_eq!(
            border_style(capabilities).fg,
            Some(Color::Rgb(148, 160, 174))
        );
    }

    #[test]
    fn diagnostic_shell_has_a_clear_empty_state_and_command_field() {
        let backend = TestBackend::new(180, 50);
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
            theme_mode: crate::ThemeMode::Auto,
            ascii: true,
        };
        let capabilities = TerminalCapabilities::detect(ColorMode::Never, true);
        let mut app = App::new(Snapshot::empty("fixture"), options, capabilities);
        app.diagnostic_open = true;

        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("render diagnostic shell");

        let rendered = rendered_text(&terminal);
        assert!(rendered.contains("COMMAND OUTPUT"));
        assert!(rendered.contains("COMMAND"));
        assert!(rendered.contains("Run a command without leaving this view."));
        assert!(rendered.contains("uptime"));
        assert!(rendered.contains("Enter to run"));
    }
}
