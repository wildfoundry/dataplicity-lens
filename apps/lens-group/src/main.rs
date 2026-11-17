#![forbid(unsafe_code)]
mod ui;
use anyhow::{Context, Result, bail};
use clap::Parser;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self},
};
use lens_reporting::{group::Page, publisher::group_client};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Parser)]
#[command(
    about = "Browse shared server groups using reports received by Dataplicity.",
    after_help = "Run as the registered publisher account: sudo -u lens-publisher lens-group\nConfigure groups in the Lens web workspace → Groups & sharing. No peer connection or local collection is used."
)]
struct Args {
    #[arg(long, default_value = "/var/lib/lens-publisher")]
    state_dir: PathBuf,
    /// Group ID from Groups & sharing. Omit to choose interactively.
    #[arg(long)]
    group: Option<String>,
    #[arg(long,default_value_t=1,value_parser=clap::value_parser!(u32).range(1..=5001))]
    page: u32,
    #[arg(long,value_parser=clap::value_parser!(u32).range(0..=100000))]
    offset: Option<u32>,
    /// Search server names across the selected group, or group names in the chooser.
    #[arg(long, default_value = "")]
    search: String,
    /// Interactive dashboard (also the default when stdout is a terminal).
    #[arg(long,conflicts_with_all=["once","json"])]
    watch: bool,
    /// Print a single human-readable page and exit.
    #[arg(long, conflicts_with = "json")]
    once: bool,
    /// Print a single machine-readable response and exit.
    #[arg(long)]
    json: bool,
    /// Preview the interface with clearly labelled sample data; no credentials or network.
    #[arg(long)]
    demo: bool,
}
#[derive(Debug, Clone)]
struct Request {
    group: Option<String>,
    offset: usize,
    search: String,
}
#[derive(Debug)]
struct View {
    page: Option<Page>,
    message: String,
    remaining: u64,
    halted: bool,
    offset: usize,
    checked_ms: Option<u64>,
    demo: bool,
    scope: Option<String>,
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn demo_view(group: Option<&str>, offset: usize, search: &str) -> View {
    let now = now_ms();
    let g = serde_json::json!({"hash_id":"11111111-1111-4111-8111-111111111111","name":"Production servers","tags":["production"],"sharing_enabled":true});
    let mut results = if group.is_none() {
        vec![g.clone()]
    } else {
        (0..18).map(|i|serde_json::json!({"hash_id":format!("demo-{i}"),"name":format!("{}-{:02}",if i<6{"build"}else if i<12{"database"}else{"web"},i+1),"service_tag":format!("DEMO{i:03}"),"status":if i==4{"delayed"}else if i==7{"expired"}else{"reporting"},"cpu_model":"Intel Xeon E5-2690 v4 × 2","cpu_threads":56,"memory_total_bytes":137438953472u64,"collected_at_ms":if i==7{None}else{Some(now.saturating_sub(if i==4{420000}else{24000+i*1000}))},"values":{"cpu":if i==7{None}else{Some((17+i*7)%100)},"memory":if i==7{None}else{Some((31+i*4)%100)},"disk":if i==7{None}else{Some(if i==2{94}else{42+i})},"processes":321+i,"services_failed":if i==2{2}else{0},"containers_running":8}})).collect::<Vec<_>>()
    };
    results.retain(|v| {
        ui::label(v, "name")
            .to_lowercase()
            .contains(&search.to_lowercase())
    });
    let total = results.len();
    let results = results.into_iter().skip(offset).take(20).collect();
    View {
        page: Some(Page {
            results,
            total,
            next: None,
            group: group.map(|_| g),
        }),
        message: "Sample data only · no network requests".into(),
        remaining: 60,
        halted: false,
        offset,
        checked_ms: Some(now),
        demo: true,
        scope: group.map(str::to_owned),
    }
}
fn received(client: &lens_reporting::group::Client, request: &Request) -> View {
    View {
        page: client.page.clone(),
        message: client.message.clone(),
        remaining: client.remaining_seconds(),
        halted: client.halted(),
        offset: request.offset,
        checked_ms: client.checked_ms,
        demo: false,
        scope: request.group.clone(),
    }
}
struct Screen;
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen, cursor::Show);
    }
}
fn run() -> Result<()> {
    let args = Args::parse();
    if args.search.chars().count() > 120 {
        bail!("Search is limited to 120 characters");
    }
    let interactive = !args.once && !args.json && io::stdout().is_terminal();
    if args.watch && !io::stdout().is_terminal() {
        bail!("--watch requires a terminal; use --once or --json for redirected output");
    }
    let mut client = if args.demo {
        None
    } else {
        Some(group_client(&args.state_dir).context("Group views need this server's Lens publisher registration. Run `sudo -u lens-publisher lens-group` after connecting the server in the Lens web workspace. Credentials must remain in the private publisher directory.")?)
    };
    let mut request = Request {
        group: args.group,
        offset: args
            .offset
            .map_or((args.page as usize - 1) * 20, |n| n as usize),
        search: args.search,
    };
    if !interactive {
        let view = if let Some(c) = client.as_mut() {
            c.search = request.search.clone();
            c.poll(request.group.as_deref(), request.offset)?;
            received(c, &request)
        } else {
            demo_view(request.group.as_deref(), request.offset, &request.search)
        };
        if view.halted || view.page.is_none() {
            bail!("{}", view.message);
        }
        if args.json {
            writeln!(io::stdout(), "{}", serde_json::to_string(&view.page)?)?;
        } else {
            write!(io::stdout(), "{}", ui::plain(&view, now_ms()))?;
        }
        return Ok(());
    }
    let (tx, rx) = mpsc::sync_channel::<Request>(1);
    let (result_tx, result_rx) = mpsc::sync_channel::<(Request, View)>(1);
    std::thread::spawn(move || {
        while let Ok(r) = rx.recv() {
            let view = if let Some(c) = client.as_mut() {
                c.search = r.search.clone();
                if c.poll(r.group.as_deref(), r.offset).is_err() {
                    c.message = "Invalid group ID or search".into();
                    c.gate.halted = true;
                    c.page = None;
                }
                received(c, &r)
            } else {
                demo_view(r.group.as_deref(), r.offset, &r.search)
            };
            if result_tx.send((r, view)).is_err() {
                break;
            }
        }
    });
    terminal::enable_raw_mode()?;
    let _screen = Screen;
    execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut dashboard = ui::Dashboard::default();
    let mut view = View {
        page: None,
        message: "Connecting to Dataplicity…".into(),
        remaining: 0,
        halted: false,
        offset: request.offset,
        checked_ms: None,
        demo: args.demo,
        scope: request.group.clone(),
    };
    let mut previous = Vec::new();
    let mut in_flight = false;
    let mut next = Instant::now();
    let mut dirty = true;
    loop {
        view.scope = request.group.clone();
        if !in_flight && !view.halted && Instant::now() >= next {
            tx.send(request.clone())?;
            in_flight = true;
        }
        if let Ok((r, result)) = result_rx.try_recv() {
            in_flight = false;
            next = Instant::now()
                .checked_add(Duration::from_secs(result.remaining.max(1)))
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(100 * 365 * 86400));
            if r.group == request.group && r.offset == request.offset && r.search == request.search
            {
                view = result;
            } else {
                view.page = None;
                view.message = "Waiting for the next permitted read.".into();
                next = Instant::now();
            }
            dirty = true;
        }
        let remaining = next.saturating_duration_since(Instant::now()).as_secs();
        if remaining != view.remaining {
            view.remaining = remaining;
            dirty = true;
        }
        if dirty {
            terminal.draw(|f| dashboard.draw(f, &view, now_ms()))?;
            dirty = false;
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let event = event::read()?;
        if matches!(event, Event::Resize(..)) {
            dirty = true;
            continue;
        }
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        dirty = true;
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            break;
        }
        if let Some(query) = dashboard.editing.as_mut() {
            match key.code {
                KeyCode::Esc => dashboard.editing = None,
                KeyCode::Enter => {
                    request.search = dashboard.editing.take().unwrap_or_default();
                    request.offset = 0;
                    previous.clear();
                    dashboard.selected = 0;
                    view.page = None;
                    next = Instant::now();
                }
                KeyCode::Backspace => {
                    query.pop();
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && query.chars().count() < 120 =>
                {
                    query.push(c)
                }
                _ => {}
            }
            continue;
        }
        if key.code == KeyCode::Char('q')
            || key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            break;
        }
        match key.code {
            KeyCode::Char('?') => dashboard.help = !dashboard.help,
            KeyCode::Down | KeyCode::Char('j') => {
                dashboard.selected = dashboard
                    .selected
                    .saturating_add(1)
                    .min(dashboard.rows(&view).len().saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => {
                dashboard.selected = dashboard.selected.saturating_sub(1)
            }
            KeyCode::PageDown => {
                dashboard.detail_scroll = dashboard.detail_scroll.saturating_add(5)
            }
            KeyCode::PageUp => dashboard.detail_scroll = dashboard.detail_scroll.saturating_sub(5),
            KeyCode::Char('s') => {
                dashboard.sort = (dashboard.sort + 1) % 4;
                dashboard.selected = 0;
            }
            KeyCode::Char('/') => dashboard.editing = Some(request.search.clone()),
            KeyCode::Enter if !dashboard.help => {
                if request.group.is_none() {
                    if let Some(row) = dashboard.selected_row(&view) {
                        request.group = Some(ui::label(row, "hash_id"));
                        request.offset = 0;
                        request.search.clear();
                        previous.clear();
                        dashboard.selected = 0;
                        view.page = None;
                        view.message = "Opening group…".into();
                        next = Instant::now();
                    }
                } else {
                    dashboard.full_detail = !dashboard.full_detail;
                }
            }
            KeyCode::Char('g') => {
                dashboard.help = false;
                dashboard.full_detail = false;
                dashboard.detail_scroll = 0;
                if request.group.is_some() {
                    request.group = None;
                    request.offset = 0;
                    request.search.clear();
                    previous.clear();
                    dashboard.selected = 0;
                    view.page = None;
                    next = Instant::now();
                }
            }
            KeyCode::Esc => {
                if dashboard.help {
                    dashboard.help = false;
                } else if dashboard.full_detail {
                    dashboard.full_detail = false;
                } else if request.group.is_some() {
                    request.group = None;
                    request.offset = 0;
                    request.search.clear();
                    previous.clear();
                    dashboard.selected = 0;
                    view.page = None;
                    next = Instant::now();
                } else {
                    break;
                }
            }
            KeyCode::Char('n') => {
                if let Some(offset) = view.page.as_ref().and_then(|p| p.next) {
                    previous.push(request.offset);
                    request.offset = offset;
                    dashboard.selected = 0;
                    view.page = None;
                    view.message = "Loading next page when permitted…".into();
                    next = Instant::now();
                }
            }
            KeyCode::Char('p') if request.offset > 0 => {
                request.offset = previous.pop().unwrap_or(request.offset.saturating_sub(20));
                dashboard.selected = 0;
                view.page = None;
                next = Instant::now();
            }
            KeyCode::Char('r') => next = Instant::now(),
            _ => {}
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
