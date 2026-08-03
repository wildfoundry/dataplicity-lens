#![forbid(unsafe_code)]

mod app;
mod render;
mod terminal;

use std::{
    fmt::Display,
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

pub fn run_tui<F, E>(initial: Snapshot, mut collect: F, options: UiOptions) -> Result<(), UiError>
where
    F: FnMut() -> Result<Snapshot, E>,
    E: Display,
{
    let mut terminal = terminal::TerminalSession::enter()?;
    let capabilities = TerminalCapabilities::detect(options.color_mode, options.ascii);
    let mut app = App::new(initial, options, capabilities);
    let mut next_refresh = Instant::now() + app.interval();
    let mut dirty = true;

    loop {
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
                    let snapshot =
                        collect().map_err(|error| UiError::Collection(error.to_string()))?;
                    app.replace_snapshot(snapshot);
                    next_refresh = Instant::now() + app.interval();
                }
                Action::Redraw => {}
                Action::None => continue,
            }
            dirty = true;
        }

        if !app.paused() && Instant::now() >= next_refresh {
            match collect() {
                Ok(snapshot) => {
                    app.replace_snapshot(snapshot);
                    app.clear_error();
                }
                Err(error) => app.set_error(error.to_string()),
            }
            next_refresh = Instant::now() + app.interval();
            dirty = true;
        }
    }
    Ok(())
}
