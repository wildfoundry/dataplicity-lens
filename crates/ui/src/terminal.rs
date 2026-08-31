use std::{
    env,
    io::{self, Stdout},
};

use crossterm::{
    cursor, execute,
    style::ResetColor,
    terminal::{
        BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate, EnterAlternateScreen,
        LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    },
};
use lens_core::{GlyphMode, TerminalEnvironment, unicode_available};
use ratatui::{Terminal, backend::CrosstermBackend};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Auto,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy)]
pub struct TerminalCapabilities {
    pub color: bool,
    pub true_color: bool,
    pub unicode: bool,
    pub light_background: bool,
}

impl TerminalCapabilities {
    pub fn detect(color_mode: ColorMode, glyph_mode: GlyphMode) -> Self {
        Self::detect_with_theme(color_mode, glyph_mode, ThemeMode::Auto)
    }

    pub fn detect_with_theme(
        color_mode: ColorMode,
        glyph_mode: GlyphMode,
        theme_mode: ThemeMode,
    ) -> Self {
        let term = env::var("TERM").unwrap_or_default();
        let color_term = env::var("COLORTERM").unwrap_or_default();
        let color = match color_mode {
            ColorMode::Always => true,
            ColorMode::Never => false,
            ColorMode::Auto => env::var_os("NO_COLOR").is_none() && term != "dumb",
        };
        Self {
            color,
            true_color: color
                && (color_term.eq_ignore_ascii_case("truecolor")
                    || color_term.eq_ignore_ascii_case("24bit")),
            unicode: unicode_available(glyph_mode, &TerminalEnvironment::detect()),
            light_background: detect_light_background(theme_mode),
        }
    }
}

fn detect_light_background(theme_mode: ThemeMode) -> bool {
    let requested = match theme_mode {
        ThemeMode::Auto => env::var("LENS_THEME").ok(),
        ThemeMode::Dark => return false,
        ThemeMode::Light => return true,
    };
    match requested.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("light") => return true,
        Some("dark") => return false,
        _ => {}
    }

    env::var("COLORFGBG")
        .ok()
        .and_then(|value| value.rsplit([';', ':']).next()?.parse::<u8>().ok())
        .is_some_and(|background| background == 7 || background >= 9)
}

pub struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl std::fmt::Debug for TerminalSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalSession")
            .finish_non_exhaustive()
    }
}

impl TerminalSession {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;
        terminal.hide_cursor()?;
        Ok(Self { terminal })
    }

    pub fn draw<F>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut ratatui::Frame<'_>),
    {
        self.terminal.draw(render).map(|_| ())
    }

    /// Draw a complete frame over every cell of the screen.
    ///
    /// Normal drawing sends only the cells that changed, so text written by another writer — kernel
    /// log output on a Linux console, most often — survives until Lens happens to change those
    /// cells. Clearing first is the only way to remove it. The clear and the frame are published as
    /// one synchronized update, so a terminal that supports that never displays the cleared screen.
    pub fn repaint<F>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut ratatui::Frame<'_>),
    {
        execute!(self.terminal.backend_mut(), BeginSynchronizedUpdate)?;
        let result = self
            .terminal
            .clear()
            .and_then(|()| self.terminal.draw(render).map(|_| ()));
        execute!(self.terminal.backend_mut(), EndSynchronizedUpdate)?;
        result
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // Some browser terminals only partially implement the alternate screen. Clear and reset
        // explicitly before leaving it so the restored shell prompt never inherits Lens cells,
        // colours or a hidden cursor.
        let _ = execute!(
            self.terminal.backend_mut(),
            ResetColor,
            Clear(ClearType::All),
            cursor::MoveTo(0, 0),
            cursor::Show,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_theme_does_not_depend_on_terminal_metadata() {
        assert!(detect_light_background(ThemeMode::Light));
        assert!(!detect_light_background(ThemeMode::Dark));
    }
}
