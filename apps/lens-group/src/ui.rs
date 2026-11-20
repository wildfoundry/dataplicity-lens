use crate::View;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap},
};
use serde_json::Value;
use time::{OffsetDateTime, macros::format_description};

const BG: Color = Color::Rgb(24, 34, 48);
const PANEL: Color = Color::Rgb(31, 44, 61);
const BORDER: Color = Color::Rgb(58, 75, 96);
const TEXT: Color = Color::Rgb(229, 236, 244);
const MUTED: Color = Color::Rgb(159, 176, 197);
const CYAN: Color = Color::Rgb(62, 204, 224);
const GREEN: Color = Color::Rgb(78, 215, 164);
const AMBER: Color = Color::Rgb(248, 190, 92);
const RED: Color = Color::Rgb(255, 126, 139);

pub fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(256).collect()
}
pub fn label(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .map(clean)
        .unwrap_or_else(|| "—".into())
}
pub fn value(v: &Value, key: &str) -> Option<f64> {
    v.get("values")?
        .get(key)?
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.)
}
fn reporting_state(row: &Value, now: u64) -> String {
    if row["expires_at_ms"]
        .as_u64()
        .is_some_and(|expiry| now >= expiry)
    {
        return "expired".into();
    }
    let status = label(row, "status");
    if status == "reporting"
        && timestamp(row)
            .zip(row["report_interval_seconds"].as_u64())
            .is_some_and(|(sample, interval)| {
                now.saturating_sub(sample)
                    > interval.saturating_mul(2).max(180).saturating_mul(1000)
            })
    {
        return "delayed".into();
    }
    status
}

fn findings(row: &Value) -> String {
    row["findings_count"]
        .as_u64()
        .map_or_else(|| "—".into(), |n| n.to_string())
}

fn page_metric(rows: &[&Value], key: &str, now: u64) -> String {
    let mut samples: Vec<_> = rows
        .iter()
        .filter(|row| reporting_state(row, now) == "reporting")
        .filter_map(|row| value(row, key))
        .collect();
    samples.sort_by(f64::total_cmp);
    let n = samples.len();
    if n == 0 {
        return format!("{key} — (n=0)");
    }
    let median = (samples[(n - 1) / 2] + samples[n / 2]) / 2.;
    format!("{key} {median:.0}/{:.0}% (n={n})", samples[n - 1])
}

fn percent(v: Option<f64>) -> String {
    v.map_or_else(|| "—".into(), |n| format!("{n:.1}%"))
}
fn metric(v: &Value, key: &str) -> String {
    let n = value(v, key);
    format!(
        "{:>6} {}",
        percent(n),
        n.map_or_else(
            || "········".into(),
            |n| {
                let bars = (n.clamp(0., 100.) / 12.5).round() as usize;
                format!("{}{}", "▰".repeat(bars), "·".repeat(8 - bars))
            }
        )
    )
}
fn pressure(n: Option<f64>) -> Color {
    match n {
        Some(n) if n >= 90. => RED,
        Some(n) if n >= 80. => AMBER,
        Some(_) => GREEN,
        None => MUTED,
    }
}
pub fn bytes(n: Option<f64>) -> String {
    n.filter(|n| n.is_finite() && *n >= 0.).map_or_else(
        || "—".into(),
        |n| {
            if n >= 1_073_741_824. {
                format!("{:.1} GiB", n / 1_073_741_824.)
            } else {
                format!("{:.0} MiB", n / 1_048_576.)
            }
        },
    )
}
pub fn age(timestamp: Option<u64>, now: u64) -> String {
    timestamp.map_or_else(
        || "Not reported".into(),
        |t| {
            if t > now.saturating_add(60_000) {
                return "Clock ahead".into();
            }
            let s = now.saturating_sub(t) / 1000;
            match s {
                0..=59 => format!("{s}s ago"),
                60..=3599 => format!("{}m ago", s / 60),
                3600..=86399 => format!("{}h {}m ago", s / 3600, s / 60 % 60),
                _ => format!("{}d ago", s / 86400),
            }
        },
    )
}
fn timestamp(v: &Value) -> Option<u64> {
    v.get("collected_at_ms").and_then(Value::as_u64)
}
fn utc(t: Option<u64>) -> String {
    t.and_then(|n| OffsetDateTime::from_unix_timestamp((n / 1000).try_into().ok()?).ok())
        .and_then(|d| {
            d.format(format_description!(
                "[year]-[month]-[day] [hour]:[minute]:[second] UTC"
            ))
            .ok()
        })
        .unwrap_or_else(|| "Not reported".into())
}
fn state(row: &Value) -> Color {
    if label(row, "status") == "reporting" {
        GREEN
    } else {
        AMBER
    }
}
fn block(title: &str) -> Block<'_> {
    Block::default()
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Style::default().fg(CYAN),
        )))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(PANEL).fg(TEXT))
}
#[derive(Debug, Default)]
pub struct Dashboard {
    pub selected: usize,
    pub sort: usize,
    pub editing: Option<String>,
    pub help: bool,
    pub full_detail: bool,
    pub detail_scroll: u16,
}
impl Dashboard {
    pub fn rows<'a>(&self, view: &'a View) -> Vec<&'a Value> {
        let mut rows: Vec<_> = view
            .page
            .as_ref()
            .map(|p| p.results.iter().collect())
            .unwrap_or_default();
        if view.page.as_ref().is_some_and(|p| p.group.is_some()) {
            rows.sort_by(|a, b| {
                match self.sort {
                    4 => {
                        return b["findings_count"]
                            .as_u64()
                            .cmp(&a["findings_count"].as_u64());
                    }
                    5 => return label(a, "status").cmp(&label(b, "status")),
                    6 => return timestamp(a).cmp(&timestamp(b)),
                    _ => {}
                }
                let metric = match self.sort {
                    1 => "cpu",
                    2 => "memory",
                    3 => "disk",
                    _ => "",
                };
                if metric.is_empty() {
                    label(a, "name")
                        .to_lowercase()
                        .cmp(&label(b, "name").to_lowercase())
                } else {
                    value(b, metric)
                        .partial_cmp(&value(a, metric))
                        .unwrap_or(std::cmp::Ordering::Equal)
                }
            })
        }
        rows
    }
    pub fn selected_row<'a>(&self, view: &'a View) -> Option<&'a Value> {
        self.rows(view).get(self.selected).copied()
    }
    pub fn draw(&self, f: &mut Frame, view: &View, now: u64) {
        let area = f.area();
        f.render_widget(
            Block::default().style(Style::default().bg(BG).fg(TEXT)),
            area,
        );
        if area.width < 48 || area.height < 14 {
            f.render_widget(
                Paragraph::new(
                    "Lens groups\nResize to at least 48 × 14, or use --once / --json.\nq quits",
                )
                .wrap(Wrap { trim: false }),
                area,
            );
            return;
        }
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(area);
        let member_view = view.scope.is_some();
        let group = view.page.as_ref().and_then(|p| p.group.as_ref());
        let name = group.map_or_else(
            || {
                if member_view {
                    "Group view".into()
                } else {
                    "Choose a group".into()
                }
            },
            |g| label(g, "name"),
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " DATAPLICITY ",
                    Style::default()
                        .fg(Color::Rgb(186, 151, 248))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("/ GROUPS   ", Style::default().fg(TEXT)),
                Span::styled(name, Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
                Span::styled(
                    if view.demo {
                        "   DEMO"
                    } else {
                        "   ORG MEMBERS ONLY"
                    },
                    Style::default().fg(GREEN),
                ),
            ]))
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(BORDER)),
            ),
            parts[0],
        );
        let rows = self.rows(view);
        let total = view.page.as_ref().map_or(0, |p| p.total);
        let summary = if member_view {
            let reporting = rows
                .iter()
                .filter(|v| reporting_state(v, now) == "reporting")
                .count();
            let high = rows
                .iter()
                .filter(|v| {
                    reporting_state(v, now) == "reporting"
                        && ["cpu", "memory", "disk"]
                            .iter()
                            .any(|k| value(v, k).is_some_and(|n| n >= 80.))
                })
                .count();
            format!(
                " {} matching servers  ·  {} on this page  ·  {reporting} reporting  ·  {high} with pressure ≥80%\n Page median/max: {}",
                total,
                rows.len(),
                ["cpu", "memory", "disk"]
                    .map(|key| page_metric(&rows, key, now))
                    .join(" · ")
            )
        } else {
            format!(
                " {total} shared groups available to this server  ·  Select a group and press Enter"
            )
        };
        f.render_widget(
            Paragraph::new(summary)
                .style(Style::default().fg(MUTED))
                .wrap(Wrap { trim: true }),
            parts[1],
        );
        if self.help {
            f.render_widget(Paragraph::new("↑/↓ or j/k   Select a server or group\nEnter        Open group / expand selected server\nEsc or g     Back to group list\n/            Search server names across this group\ns            Cycle the page sort column\nn / p        Next / previous page\nr            Refresh when permitted\n?            Show / close this help\nq            Quit\n\nOnly Dataplicity-received reports are shown. Values can be older than the last successful connection. Unknown readings are never zero.\n\nBackground refresh: at least 60 seconds. Navigation: paced reads; no prefetch. Backoff and server Retry-After always apply.").block(block("Keyboard & data freshness")).wrap(Wrap{trim:false}),parts[2]);
        } else if self.full_detail && member_view {
            if let Some(row) = self.selected_row(view) {
                self.detail(f, parts[2], row, now);
            } else {
                f.render_widget(
                    Paragraph::new(view.message.as_str()).block(block("Server details")),
                    parts[2],
                );
            }
        } else {
            let widths = if !member_view {
                vec![Constraint::Min(20), Constraint::Length(37)]
            } else if area.width >= 115 {
                vec![
                    Constraint::Min(17),
                    Constraint::Length(11),
                    Constraint::Length(16),
                    Constraint::Length(16),
                    Constraint::Length(16),
                    Constraint::Length(9),
                    Constraint::Length(9),
                ]
            } else {
                vec![
                    Constraint::Min(16),
                    Constraint::Length(10),
                    Constraint::Length(7),
                    Constraint::Length(7),
                    Constraint::Length(7),
                ]
            };
            let headers = if !member_view {
                vec!["Group", "ID"]
            } else if area.width >= 115 {
                vec![
                    "Server", "Status", "CPU", "Memory", "Storage", "Findings", "Age",
                ]
            } else {
                vec!["Server", "Status", "CPU", "RAM", "Disk"]
            };
            let table_rows: Vec<Row> = rows
                .iter()
                .map(|v| {
                    let cells = if !member_view {
                        vec![
                            Cell::from(label(v, "name")),
                            Cell::from(label(v, "hash_id")).style(Style::default().fg(MUTED)),
                        ]
                    } else {
                        let mut c = vec![
                            Cell::from(if value(v, "services_failed").is_some_and(|n| n > 0.) {
                                format!("! {}", label(v, "name"))
                            } else {
                                label(v, "name")
                            }),
                            Cell::from(reporting_state(v, now))
                                .style(Style::default().fg(state(v))),
                        ];
                        for k in ["cpu", "memory", "disk"] {
                            c.push(
                                Cell::from(if area.width >= 115 {
                                    metric(v, k)
                                } else {
                                    percent(value(v, k))
                                })
                                .style(Style::default().fg(pressure(value(v, k)))),
                            );
                        }
                        if area.width >= 115 {
                            c.push(Cell::from(findings(v)));
                            c.push(
                                Cell::from(
                                    timestamp(v)
                                        .map_or_else(|| "—".into(), |_| age(timestamp(v), now)),
                                )
                                .style(Style::default().fg(MUTED)),
                            );
                        }
                        c
                    };
                    Row::new(cells).height(2)
                })
                .collect();
            let title = if member_view {
                format!(
                    "Servers · {} sort on this page",
                    [
                        "name", "CPU", "memory", "storage", "findings", "status", "age"
                    ][self.sort]
                )
            } else {
                "Your shared groups".into()
            };
            let table = Table::new(table_rows, widths)
                .header(
                    Row::new(headers)
                        .style(Style::default().fg(MUTED))
                        .height(2),
                )
                .block(block(&title))
                .row_highlight_style(
                    Style::default()
                        .bg(Color::Rgb(67, 53, 96))
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▸ ");
            let mut state = TableState::default().with_selected(if rows.is_empty() {
                None
            } else {
                Some(self.selected.min(rows.len() - 1))
            });
            f.render_stateful_widget(table, parts[2], &mut state);
            if rows.is_empty() {
                let inner = Rect::new(
                    parts[2].x + 2,
                    parts[2].y + 4,
                    parts[2].width.saturating_sub(4),
                    parts[2].height.saturating_sub(5),
                );
                let message = if view.page.is_none() {
                    view.message.clone()
                } else if member_view {
                    "No servers match this search. Press / to change it or Esc to return to groups."
                        .into()
                } else {
                    "No shared groups yet. In the Lens web workspace, add tags to this server, create a matching group and enable member sharing. Only groups in this organisation are available.".into()
                };
                f.render_widget(
                    Paragraph::new(message)
                        .wrap(Wrap { trim: true })
                        .style(Style::default().fg(MUTED)),
                    inner,
                );
            }
        }
        let checked = age(view.checked_ms, now);
        let range = if rows.is_empty() {
            "No results".into()
        } else {
            format!(
                "{}–{} of {}",
                view.offset + 1,
                view.offset + rows.len(),
                total
            )
        };
        let message = if let Some(query) = &self.editing {
            format!(" Search server names: {query}▏  · Enter apply · Esc cancel")
        } else {
            format!(
                " {range}  · Checked {checked}  · Next read {}s\n {}",
                view.remaining, view.message
            )
        };
        f.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(if view.halted { RED } else { MUTED }))
                .wrap(Wrap { trim: true }),
            parts[3],
        );
        f.render_widget(
            Paragraph::new(
                " ↑↓ select  Enter open  / search  s sort page  n/p page  g groups  ? help  q quit",
            )
            .style(Style::default().fg(CYAN)),
            parts[4],
        );
    }
    fn detail(&self, f: &mut Frame, area: Rect, v: &Value, now: u64) {
        let count =
            |k: &str| value(v, k).map_or_else(|| "Not reported".into(), |n| format!("{n:.0}"));
        let threads = v
            .get("cpu_threads")
            .and_then(Value::as_u64)
            .map_or_else(|| "—".into(), |n| n.to_string());
        let lines = vec![
            Line::styled(
                label(v, "name"),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
            Line::styled(reporting_state(v, now), Style::default().fg(state(v))),
            Line::from(""),
            Line::from(format!("Findings     {}", findings(v))),
            Line::from(if v["partial"] == true {
                "Partial sample"
            } else {
                ""
            }),
            Line::from(format!("Service tag  {}", label(v, "service_tag"))),
            Line::from(""),
            Line::styled("CPU", Style::default().fg(CYAN)),
            Line::from(label(v, "cpu_model")),
            Line::from(format!(
                "{threads} threads · {} busy",
                percent(value(v, "cpu"))
            )),
            Line::from(""),
            Line::styled("MEMORY", Style::default().fg(CYAN)),
            Line::from(format!(
                "{} installed · {} used",
                bytes(v.get("memory_total_bytes").and_then(Value::as_f64)),
                percent(value(v, "memory"))
            )),
            Line::from(""),
            Line::styled("STORAGE & ACTIVITY", Style::default().fg(CYAN)),
            Line::from(format!(
                "Highest persistent disk use  {}",
                percent(value(v, "disk"))
            )),
            Line::from(format!("Processes          {}", count("processes"))),
            Line::styled(
                format!("Failed services    {}", count("services_failed")),
                Style::default().fg(if value(v, "services_failed").is_some_and(|n| n > 0.) {
                    RED
                } else {
                    TEXT
                }),
            ),
            Line::from(format!(
                "Running containers {}",
                count("containers_running")
            )),
            Line::from(""),
            Line::styled("REPORT FRESHNESS", Style::default().fg(CYAN)),
            Line::from(age(timestamp(v), now)),
            Line::from(utc(timestamp(v))),
            Line::from(""),
            Line::styled(
                "Reported to Dataplicity; no peer connection.",
                Style::default().fg(MUTED),
            ),
        ];
        f.render_widget(
            Paragraph::new(lines)
                .block(block("Selected server · Enter full screen · PgUp/PgDn"))
                .scroll((self.detail_scroll, 0))
                .wrap(Wrap { trim: false }),
            area,
        );
    }
}
pub fn plain(view: &View, now: u64) -> String {
    let mut s = format!(
        "Lens groups · Dataplicity-reported data{}\n{}\n",
        if view.demo { " · DEMO" } else { "" },
        view.message
    );
    if let Some(p) = &view.page {
        if let Some(g) = &p.group {
            s.push_str(&format!(
                "{} · {} matching servers\n",
                label(g, "name"),
                p.total
            ));
        }
        for v in &p.results {
            if p.group.is_none() {
                s.push_str(&format!("{}  {}\n", label(v, "hash_id"), label(v, "name")));
            } else {
                s.push_str(&format!("\n{} [{}] · service tag {}\nCPU {} · Memory {} · Storage {}\n{} · {} threads · {} RAM\nProcesses {} · Failed services {} · Running containers {}\nReport {} · {}\n",label(v,"name"),reporting_state(v,now),label(v,"service_tag"),percent(value(v,"cpu")),percent(value(v,"memory")),percent(value(v,"disk")),label(v,"cpu_model"),v.get("cpu_threads").unwrap_or(&Value::Null),bytes(v.get("memory_total_bytes").and_then(Value::as_f64)),percent_count(v,"processes"),percent_count(v,"services_failed"),percent_count(v,"containers_running"),age(timestamp(v),now),utc(timestamp(v))));
                s.push_str(&format!("Findings {}\n", findings(v)));
            }
        }
        s.push_str(&format!(
            "\n{} results of {} · More: {}\n",
            p.results.len(),
            p.total,
            p.next.is_some()
        ));
    }
    s
}
fn percent_count(v: &Value, k: &str) -> String {
    value(v, k).map_or_else(|| "—".into(), |n| format!("{n:.0}"))
}
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    #[test]
    fn ages_handle_unknown_future_and_days() {
        assert_eq!(age(None, 60000), "Not reported");
        assert_eq!(age(Some(0), 120000), "2m ago");
        assert_eq!(age(Some(200000), 0), "Clock ahead");
        assert_eq!(age(Some(0), 172800000), "2d ago");
    }
    #[test]
    fn dashboard_fits_narrow_and_wide_terminals() {
        for (w, h) in [(40, 10), (60, 18), (100, 30), (160, 40)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            let v = crate::demo_view(Some("demo"), 0, "");
            t.draw(|f| Dashboard::default().draw(f, &v, crate::now_ms()))
                .unwrap();
            let text = t
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(text.contains("Lens groups") || text.contains("DATAPLICITY"));
            assert!(!text.contains("ms since epoch"));
        }
    }
}
