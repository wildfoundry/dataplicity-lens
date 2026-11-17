#![forbid(unsafe_code)]
use anyhow::{Result, bail};
use clap::Parser;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{self, ClearType},
};
use lens_reporting::{group::Page, publisher::group_client};
use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

#[derive(Debug, Parser)]
#[command(
    about = "View shared group summaries received by Dataplicity. Never collects locally or contacts peer servers."
)]
struct Args {
    #[arg(long, default_value = "/var/lib/lens-publisher")]
    state_dir: PathBuf,
    /// Group ID from Lens → Groups & sharing; omit to list available groups.
    #[arg(long)]
    group: Option<String>,
    /// One bounded page, starting at 1. Never downloads the entire group.
    #[arg(long, default_value_t=1, value_parser=clap::value_parser!(u32).range(1..=5001))]
    page: u32,
    /// Exact continuation offset from a previous JSON response.
    #[arg(long, value_parser=clap::value_parser!(u32).range(0..=100000))]
    offset: Option<u32>,
    /// Refresh in the terminal; q exits, n/p changes page without bypassing cooldown.
    #[arg(long)]
    watch: bool,
    #[arg(long)]
    json: bool,
}
#[derive(Debug)]
struct View {
    page: Option<Page>,
    message: String,
    remaining: u64,
    halted: bool,
    offset: usize,
}
fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect()
}
fn label(value: &serde_json::Value, key: &str) -> String {
    clean(value.get(key).and_then(|v| v.as_str()).unwrap_or("—"))
}
fn reading(value: &serde_json::Value, key: &str) -> String {
    match value
        .get("values")
        .and_then(|v| v.get(key))
        .and_then(|v| v.as_f64())
        .filter(|v| v.is_finite() && *v >= 0.0)
    {
        Some(v) => format!(
            "{:>5.1}% {}",
            v,
            "▰".repeat((v.clamp(0.0, 100.0) / 10.0).round() as usize)
        ),
        None => "    —".into(),
    }
}
fn render(view: &View) -> String {
    let mut text = format!(
        "Lens group · Dataplicity-reported data\n{}\n\n",
        view.message
    );
    if let Some(page) = &view.page {
        if let Some(group) = &page.group {
            text.push_str(&format!(
                "{} · {} servers\n",
                label(group, "name"),
                page.total
            ));
        }
        for row in &page.results {
            if page.group.is_none() {
                text.push_str(&format!(
                    "{}  {}\n",
                    label(row, "hash_id"),
                    label(row, "name")
                ));
            } else {
                text.push_str(&format!("{}  [{}]  service tag {}\n  CPU {}  Memory {}\n  Report collected at: {} ms since epoch\n",label(row,"name"),label(row,"status"),label(row,"service_tag"),reading(row,"cpu"),reading(row,"memory"),row.get("collected_at_ms").map_or("—".into(),|v|v.to_string())));
            }
        }
        text.push_str(&format!(
            "\nStarting at result {} · {} total · More: {}\n",
            view.offset + 1,
            page.total,
            page.next.is_some()
        ));
    }
    text.push_str(&format!(
        "\nNext read in {}s · q quit · n/p page\n",
        view.remaining
    ));
    text
}
struct Terminal;
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen, cursor::Show);
    }
}
fn run() -> Result<()> {
    let args = Args::parse();
    if args.watch && (args.json || !io::stdout().is_terminal()) {
        bail!("--watch requires a terminal and cannot be combined with --json");
    }
    let mut client = group_client(&args.state_dir)?;
    let mut offset = args
        .offset
        .map(|value| value as usize)
        .unwrap_or((args.page as usize - 1) * 20);
    let mut previous_offsets = Vec::new();
    if !args.watch {
        client.poll(args.group.as_deref(), offset)?;
        if client.halted() || client.page.is_none() {
            bail!("{}", client.message);
        }
        if args.json {
            writeln!(io::stdout(), "{}", serde_json::to_string(&client.page)?)?;
        } else {
            write!(
                io::stdout(),
                "{}",
                render(&View {
                    page: client.page.clone(),
                    message: client.message.clone(),
                    remaining: client.remaining_seconds(),
                    halted: client.halted(),
                    offset
                })
            )?;
        }
        return Ok(());
    }
    let (tx, rx) = mpsc::sync_channel::<usize>(1);
    let (result_tx, result_rx) = mpsc::sync_channel::<View>(1);
    let group = args.group;
    std::thread::spawn(move || {
        while let Ok(offset) = rx.recv() {
            if client.poll(group.as_deref(), offset).is_err() {
                client.message = "Invalid group ID or page".into();
                client.gate.halted = true;
                client.page = None;
            }
            let view = View {
                page: client.page.clone(),
                message: client.message.clone(),
                remaining: client.remaining_seconds(),
                halted: client.halted(),
                offset,
            };
            if result_tx.send(view).is_err() {
                break;
            }
        }
    });
    terminal::enable_raw_mode()?;
    let _guard = Terminal;
    execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide)?;
    let mut scroll = 0usize;
    let mut in_flight = false;
    let mut next = Instant::now();
    let mut view = View {
        page: None,
        message: "Connecting to Dataplicity…".into(),
        remaining: 0,
        halted: false,
        offset,
    };
    loop {
        if !in_flight && !view.halted && Instant::now() >= next {
            tx.send(offset)?;
            in_flight = true;
        }
        if let Ok(result) = result_rx.try_recv() {
            in_flight = false;
            next = Instant::now()
                .checked_add(Duration::from_secs(result.remaining.max(1)))
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(100 * 365 * 86400));
            if result.offset == offset {
                view = result;
            } else {
                view.page = None;
                view.message = "Page changed; waiting for the next permitted read.".into();
            }
        }
        view.remaining = next.saturating_duration_since(Instant::now()).as_secs();
        execute!(
            io::stdout(),
            cursor::MoveTo(0, 0),
            terminal::Clear(ClearType::All)
        )?;
        // Bound rendering to terminal height; never queue an unbounded scrollback.
        let height = terminal::size()?.1 as usize;
        for line in render(&view)
            .lines()
            .skip(scroll)
            .take(height.saturating_sub(1))
        {
            write!(io::stdout(), "{line}\r\n")?;
        }
        io::stdout().flush()?;
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('c')
                            if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            break;
                        }
                        KeyCode::Down => {
                            scroll = scroll.saturating_add(1).min(
                                render(&view)
                                    .lines()
                                    .count()
                                    .saturating_sub(height.saturating_sub(1)),
                            );
                        }
                        KeyCode::Up => {
                            scroll = scroll.saturating_sub(1);
                        }
                        KeyCode::Char('n')
                            if view.page.as_ref().is_some_and(|p| p.next.is_some()) =>
                        {
                            previous_offsets.push(offset);
                            offset = view.page.as_ref().unwrap().next.unwrap();
                            scroll = 0;
                            view.page = None;
                            view.message = "Waiting for the next permitted page read.".into();
                        }
                        KeyCode::Char('p') if offset > 0 => {
                            offset = previous_offsets.pop().unwrap_or(offset.saturating_sub(20));
                            scroll = 0;
                            view.page = None;
                            view.message = "Waiting for the next permitted page read.".into();
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        if error.chain().any(|e| {
            e.downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
        }) {
            return;
        }
        eprintln!("lens-group: {error:#}");
        std::process::exit(1);
    }
}
