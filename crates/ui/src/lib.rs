#![forbid(unsafe_code)]

mod app;
mod render;
mod terminal;

use std::{
    env,
    fmt::Display,
    process::Command,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use app::{Action, App, ProcessSignal};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use lens_core::{GlyphMode, GroupMode, ProcessFilter, SortDirection, SortKey, TerminalEnvironment};
use lens_model::Snapshot;
use thiserror::Error;

pub use terminal::{ColorMode, TerminalCapabilities, ThemeMode};

#[derive(Debug, Clone)]
pub struct UiOptions {
    pub interval: Duration,
    pub sort_key: SortKey,
    pub sort_direction: SortDirection,
    pub group: GroupMode,
    pub filter: ProcessFilter,
    pub limit: Option<usize>,
    pub history_length: usize,
    pub color_mode: ColorMode,
    pub theme_mode: ThemeMode,
    pub glyphs: GlyphMode,
}

#[derive(Debug, Error)]
pub enum UiError {
    #[error("terminal error: {0}")]
    Terminal(#[from] std::io::Error),
    #[error("collection error: {0}")]
    Collection(String),
}

type CollectionResult = Result<Snapshot, String>;

fn spawn_collection_worker<F, E>(mut collect: F) -> (Sender<()>, Receiver<CollectionResult>)
where
    F: FnMut() -> Result<Snapshot, E> + Send + 'static,
    E: Display + 'static,
{
    let (request_tx, request_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    thread::spawn(move || {
        while request_rx.recv().is_ok() {
            let result = collect().map_err(|error| error.to_string());
            if result_tx.send(result).is_err() {
                break;
            }
        }
    });
    (request_tx, result_rx)
}

pub fn run_tui<F, E>(initial: Snapshot, collect: F, options: UiOptions) -> Result<(), UiError>
where
    F: FnMut() -> Result<Snapshot, E> + Send + 'static,
    E: Display + 'static,
{
    let mut terminal = terminal::TerminalSession::enter()?;
    let environment = TerminalEnvironment::detect();
    let capabilities = TerminalCapabilities::detect_with_theme(
        options.color_mode,
        options.glyphs,
        options.theme_mode,
    );
    let kernel_log_shares_screen = environment.shares_screen_with_kernel_log();
    let mut app = App::new(initial, options, capabilities);
    let (request_tx, result_rx) = spawn_collection_worker(collect);
    let (diagnostic_tx, diagnostic_rx) = mpsc::channel();
    let (process_action_tx, process_action_rx) = mpsc::channel();
    let mut next_refresh = Instant::now() + app.interval();
    let mut next_clock = Instant::now() + Duration::from_secs(1);
    let mut refresh_in_flight = false;
    let mut collector_available = true;
    let mut dirty = true;
    let mut repaint = true;

    loop {
        if let Ok(output) = diagnostic_rx.try_recv() {
            app.finish_diagnostic(output);
            dirty = true;
        }
        if let Ok(output) = process_action_rx.try_recv() {
            app.finish_process_action(output);
            if !refresh_in_flight && collector_available && request_tx.send(()).is_ok() {
                refresh_in_flight = true;
                app.set_collecting(true);
            }
            dirty = true;
        }
        if refresh_in_flight {
            match result_rx.try_recv() {
                Ok(Ok(snapshot)) => {
                    app.replace_snapshot(snapshot);
                    app.clear_error();
                    app.set_collecting(false);
                    refresh_in_flight = false;
                    next_refresh = Instant::now() + app.interval();
                    dirty = true;
                }
                Ok(Err(error)) => {
                    app.set_error(error);
                    app.set_collecting(false);
                    refresh_in_flight = false;
                    next_refresh = Instant::now() + app.interval();
                    dirty = true;
                }
                Err(TryRecvError::Disconnected) => {
                    app.set_error("process collector stopped unexpectedly".to_owned());
                    app.set_collecting(false);
                    refresh_in_flight = false;
                    collector_available = false;
                    dirty = true;
                }
                Err(TryRecvError::Empty) => {}
            }
        }

        if dirty {
            let render = |frame: &mut ratatui::Frame<'_>| {
                render::draw(frame, &mut app);
                if !capabilities.unicode {
                    render::enforce_ascii(frame.buffer_mut());
                }
            };
            if repaint {
                terminal.repaint(render)?;
            } else {
                terminal.draw(render)?;
            }
            dirty = false;
            repaint = false;
        }

        let now = Instant::now();
        let until_refresh = next_refresh.saturating_duration_since(now);
        let until_clock = next_clock.saturating_duration_since(now);
        let timeout = until_refresh
            .min(until_clock)
            .min(Duration::from_millis(100));
        if event::poll(timeout)? {
            let event = event::read()?;
            if matches!(event, Event::Resize(_, _)) {
                dirty = true;
                continue;
            }
            let Event::Key(key) = event else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                break;
            }
            if key.code == KeyCode::Char('l') && key.modifiers.contains(KeyModifiers::CONTROL) {
                dirty = true;
                repaint = true;
                continue;
            }
            match app.handle_key(key) {
                Action::Quit => break,
                Action::Refresh => {
                    if !refresh_in_flight && collector_available {
                        if request_tx.send(()).is_ok() {
                            refresh_in_flight = true;
                            app.set_collecting(true);
                        } else {
                            app.set_error("process collector stopped unexpectedly".to_owned());
                            collector_available = false;
                        }
                    }
                    next_refresh = Instant::now() + app.interval();
                }
                Action::RunDiagnostic(command) => {
                    let sender = diagnostic_tx.clone();
                    thread::spawn(move || {
                        let _ = sender.send(run_diagnostic_command(&command));
                    });
                }
                Action::ExecuteProcessSignal {
                    signal,
                    pid,
                    start_time_ticks,
                } => {
                    let sender = process_action_tx.clone();
                    thread::spawn(move || {
                        let _ =
                            sender.send(run_process_action_command(signal, pid, start_time_ticks));
                    });
                }
                Action::Redraw => {}
                Action::None => continue,
            }
            dirty = true;
        }

        if Instant::now() >= next_clock {
            next_clock = Instant::now() + Duration::from_secs(1);
            dirty = true;
            // A console shared with the kernel log needs the whole screen rewritten to remove
            // messages printed over the frame; elsewhere nothing overwrites Lens.
            repaint |= kernel_log_shares_screen;
        }

        if !app.paused()
            && !refresh_in_flight
            && collector_available
            && Instant::now() >= next_refresh
        {
            if request_tx.send(()).is_ok() {
                refresh_in_flight = true;
                app.set_collecting(true);
            } else {
                app.set_error("process collector stopped unexpectedly".to_owned());
                collector_available = false;
            }
            next_refresh = Instant::now() + app.interval();
            dirty = true;
        }
    }
    Ok(())
}

fn run_process_action_command(signal: ProcessSignal, pid: u32, start_time_ticks: u64) -> String {
    let executable = match env::current_exe() {
        Ok(executable) => executable,
        Err(error) => return format!("Unable to locate lens-top: {error}"),
    };
    match Command::new(executable)
        .args([
            "--signal",
            signal.cli_name(),
            "--pid",
            &pid.to_string(),
            "--expect-start-ticks",
            &start_time_ticks.to_string(),
            "--yes",
        ])
        .output()
    {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            let text = sanitise_terminal_output(&text);
            if output.status.success() {
                text.trim().to_owned()
            } else {
                format!("Action failed: {}", text.trim())
            }
        }
        Err(error) => format!("Unable to run process action: {error}"),
    }
}

fn run_diagnostic_command(command: &str) -> String {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
    match Command::new(shell).args(["-lc", command]).output() {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            let text = sanitise_terminal_output(&text);
            if text.trim().is_empty() {
                format!("[exit {}]", output.status.code().unwrap_or_default())
            } else if output.status.success() {
                text
            } else {
                format!(
                    "{text}\n[exit {}]",
                    output.status.code().unwrap_or_default()
                )
            }
        }
        Err(error) => format!("Unable to start shell: {error}"),
    }
}

fn sanitise_terminal_output(text: &str) -> String {
    text.chars()
        .filter(|character| {
            matches!(character, '\n' | '\t') || (!character.is_control() && *character != '\u{1b}')
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_worker_does_not_block_the_ui_thread() {
        let (request, results) = spawn_collection_worker(|| {
            thread::sleep(Duration::from_millis(100));
            Ok::<_, &'static str>(Snapshot::empty("fixture"))
        });

        request.send(()).expect("request collection");
        assert!(matches!(
            results.recv_timeout(Duration::from_millis(10)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(results.recv_timeout(Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn diagnostic_command_captures_shell_output() {
        assert_eq!(run_diagnostic_command("printf lens"), "lens");
    }
}
