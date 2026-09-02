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
