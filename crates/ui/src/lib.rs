#![forbid(unsafe_code)]

mod app;
mod render;
mod terminal;

use std::{
    fmt::Display,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use app::{Action, App};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use lens_core::{GroupMode, ProcessFilter, SortDirection, SortKey};
use lens_model::Snapshot;
use thiserror::Error;

pub use terminal::{ColorMode, TerminalCapabilities};

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
    pub ascii: bool,
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
    let capabilities = TerminalCapabilities::detect(options.color_mode, options.ascii);
    let mut app = App::new(initial, options, capabilities);
    let (request_tx, result_rx) = spawn_collection_worker(collect);
    let mut next_refresh = Instant::now() + app.interval();
    let mut refresh_in_flight = false;
    let mut collector_available = true;
    let mut dirty = true;

    loop {
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
            terminal.draw(|frame| render::draw(frame, &mut app))?;
            dirty = false;
        }

        let now = Instant::now();
        let until_refresh = next_refresh.saturating_duration_since(now);
        let timeout = until_refresh.min(Duration::from_millis(100));
        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
        {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                break;
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
                Action::Redraw => {}
                Action::None => continue,
            }
            dirty = true;
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
}
