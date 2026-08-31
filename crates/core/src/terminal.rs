//! Shared detection of what the attached terminal can actually display.
//!
//! Lens draws box-drawing characters, block sparklines and arrow keycaps. Those glyphs are missing
//! from the Linux virtual console font unless the console is running in UTF-8 mode, so a Raspberry
//! Pi or embedded image booted without a UTF-8 locale renders every frame as mojibake. Detection
//! lives here so the cockpit, the specialists and `lens-top` all reach the same conclusion.

use std::{
    env,
    io::{self, IsTerminal},
};

/// Whether Lens should draw Unicode glyphs, ASCII replacements, or decide for itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GlyphMode {
    #[default]
    Auto,
    Unicode,
    Ascii,
}

/// The terminal facts glyph selection depends on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalEnvironment {
    /// Value of `TERM`, empty when unset.
    pub term: String,
    /// Effective `LC_ALL` / `LC_CTYPE` / `LANG` value, empty when none are set.
    pub locale: String,
    /// `Some(true)` when an environment variable explicitly asks for ASCII output.
    pub ascii_requested: Option<bool>,
    /// Whether Lens is drawing to a terminal rather than a pipe or file.
    pub interactive: bool,
}

impl TerminalEnvironment {
    /// Read the current process environment.
    pub fn detect() -> Self {
        Self {
            term: env::var("TERM").unwrap_or_default(),
            locale: first_set(&["LC_ALL", "LC_CTYPE", "LANG"]),
            ascii_requested: env::var("LENS_ASCII")
                .ok()
                .as_deref()
                .and_then(parse_boolean),
            interactive: io::stdout().is_terminal(),
        }
    }
}

/// Resolve whether Unicode glyphs may be used.
///
/// `Auto` keeps Unicode unless the terminal is one that cannot be trusted with it: `TERM=dumb`, an
/// unidentified terminal, or a Linux/serial console without a UTF-8 locale. Output that is not
/// going to a terminal stays Unicode so piped text remains byte-stable regardless of the caller's
/// locale.
#[must_use]
pub fn unicode_available(mode: GlyphMode, environment: &TerminalEnvironment) -> bool {
    match mode {
        GlyphMode::Unicode => true,
        GlyphMode::Ascii => false,
        GlyphMode::Auto => {
            if let Some(ascii) = environment.ascii_requested {
                return !ascii;
            }
            if !environment.interactive {
                return true;
            }
            if environment.term.is_empty() || environment.term == "dumb" {
                return false;
            }
            if !is_legacy_console(&environment.term) {
                return true;
            }
            locale_is_utf8(&environment.locale)
        }
    }
}

/// Parse the boolean spellings accepted by Lens environment variables.
#[must_use]
pub fn parse_boolean(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" | "" => Some(false),
        _ => None,
    }
}

/// Terminals whose glyph coverage depends on the console being in UTF-8 mode.
fn is_legacy_console(term: &str) -> bool {
    term == "linux"
        || term == "console"
        || term == "ansi"
        || term.starts_with("linux-")
        || term.starts_with("vt")
        || term.ends_with(".linux")
}

fn locale_is_utf8(locale: &str) -> bool {
    let locale = locale.to_ascii_lowercase();
    locale.contains("utf-8") || locale.contains("utf8")
}

fn first_set(names: &[&str]) -> String {
    names
        .iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.is_empty()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn console(locale: &str) -> TerminalEnvironment {
        TerminalEnvironment {
            term: "linux".to_owned(),
            locale: locale.to_owned(),
            ascii_requested: None,
            interactive: true,
        }
    }

    #[test]
    fn linux_console_without_a_utf8_locale_falls_back_to_ascii() {
        assert!(!unicode_available(GlyphMode::Auto, &console("")));
        assert!(!unicode_available(GlyphMode::Auto, &console("C")));
        assert!(!unicode_available(
            GlyphMode::Auto,
            &console("en_GB.iso88591")
        ));
    }

    #[test]
    fn linux_console_with_a_utf8_locale_keeps_unicode() {
        assert!(unicode_available(GlyphMode::Auto, &console("en_GB.UTF-8")));
        assert!(unicode_available(GlyphMode::Auto, &console("C.utf8")));
    }

    #[test]
    fn graphical_terminals_keep_unicode_whatever_the_locale() {
        let environment = TerminalEnvironment {
            term: "xterm-256color".to_owned(),
            ..console("")
        };
        assert!(unicode_available(GlyphMode::Auto, &environment));
    }

    #[test]
    fn unknown_and_dumb_terminals_fall_back_to_ascii() {
        for term in ["", "dumb"] {
            let environment = TerminalEnvironment {
                term: term.to_owned(),
                ..console("en_GB.UTF-8")
            };
            assert!(!unicode_available(GlyphMode::Auto, &environment));
        }
    }

    #[test]
    fn piped_output_stays_unicode_so_text_remains_stable() {
        let environment = TerminalEnvironment {
            interactive: false,
            ..console("")
        };
        assert!(unicode_available(GlyphMode::Auto, &environment));
    }

    #[test]
    fn explicit_requests_win_over_detection() {
        let environment = TerminalEnvironment {
            ascii_requested: Some(true),
            ..console("en_GB.UTF-8")
        };
        assert!(!unicode_available(GlyphMode::Auto, &environment));
        assert!(unicode_available(GlyphMode::Unicode, &environment));

        let environment = TerminalEnvironment {
            ascii_requested: Some(false),
            ..console("")
        };
        assert!(unicode_available(GlyphMode::Auto, &environment));
        assert!(!unicode_available(GlyphMode::Ascii, &environment));
    }

    #[test]
    fn boolean_spellings_are_parsed_leniently() {
        assert_eq!(parse_boolean("1"), Some(true));
        assert_eq!(parse_boolean(" YES "), Some(true));
        assert_eq!(parse_boolean("off"), Some(false));
        assert_eq!(parse_boolean(""), Some(false));
        assert_eq!(parse_boolean("maybe"), None);
    }
}
