// Colour, and the two notices that use it.
//
// The change set is data — `list` paths get piped into another tool, so they
// stay bytes on stdout whatever the terminal is. Everything here is for the
// person watching instead, which is why each stream is tested separately:
// notices go to stderr and `status` to stdout, and one being a pipe says
// nothing about the other.

use clap::builder::styling::{AnsiColor, Effects, Style};
use std::io::IsTerminal;
use std::path::Path;

pub struct Ink(bool);

impl Ink {
    fn new(is_terminal: bool) -> Self {
        Ink(is_terminal && std::env::var_os("NO_COLOR").is_none())
    }

    pub fn stdout() -> Self {
        Ink::new(std::io::stdout().is_terminal())
    }

    pub fn stderr() -> Self {
        Ink::new(std::io::stderr().is_terminal())
    }

    fn wrap(&self, style: Style, text: &str) -> String {
        if self.0 {
            format!("{}{text}{}", style.render(), style.render_reset())
        } else {
            text.to_string()
        }
    }

    pub fn bold(&self, text: &str) -> String {
        self.wrap(Style::new().effects(Effects::BOLD), text)
    }

    pub fn dim(&self, text: &str) -> String {
        self.wrap(Style::new().effects(Effects::DIMMED), text)
    }

    pub fn red(&self, text: &str) -> String {
        self.wrap(AnsiColor::Red.on_default(), text)
    }

    pub fn green(&self, text: &str) -> String {
        self.wrap(AnsiColor::Green.on_default(), text)
    }

    pub fn yellow(&self, text: &str) -> String {
        self.wrap(AnsiColor::Yellow.on_default(), text)
    }

    pub fn cyan(&self, text: &str) -> String {
        self.wrap(AnsiColor::Cyan.on_default(), text)
    }
}

// How long ago a unix-seconds instant was, at the coarsest unit that still says
// something. A clock that moved backwards since the mark reads as "just now"
// rather than a negative age.
pub fn ago(unix_secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let secs = now.saturating_sub(unix_secs);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

pub fn removed(path: &Path) -> String {
    let ink = Ink::stderr();
    format!(
        "{} {}",
        ink.dim("removed"),
        ink.bold(&path.display().to_string())
    )
}

pub fn marked_files(count: usize) -> String {
    let ink = Ink::stderr();
    format!(
        "{} {} {}",
        ink.dim("marked"),
        ink.green(&count.to_string()),
        ink.dim("files")
    )
}
