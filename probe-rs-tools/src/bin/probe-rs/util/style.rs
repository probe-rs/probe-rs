//! Named text styles for the CLI and for the DAP REPL.

use probe_rs_debug::{ColumnType, SourceLocation};
use ratatui::crossterm::style::Stylize;
use std::env::VarError;
use std::fmt::Display;

pub(crate) fn probe_rs_color_enabled() -> bool {
    matches!(
        std::env::var("PROBE_RS_COLOR").as_deref(),
        Err(VarError::NotPresent) | Ok("true" | "1" | "yes" | "on")
    )
}

/// Defines a named style as a `Display` wrapper.
///
/// The style expression lives in one place. By default, each wrapper consults
/// `probe_rs_color_enabled()` (i.e. the `PROBE_RS_COLOR` env var) when rendering.
/// Call sites with a different rendering context — e.g. a DAP handler whose
/// output is interpreted by a remote client — can override that decision with
/// `.colorize(bool)` without having to know about `PROBE_RS_COLOR` at all.
macro_rules! styled {
    ($name:ident($var:ident) => $style:expr) => {
        pub struct $name<S: AsRef<str>> {
            value: S,
            colorize: Option<bool>,
        }

        impl<S: AsRef<str>> $name<S> {
            pub fn new(value: S) -> Self {
                Self {
                    value,
                    colorize: None,
                }
            }

            /// Explicitly turn ANSI styling on/off, bypassing the `PROBE_RS_COLOR` default.
            #[allow(dead_code)]
            pub fn colorize(mut self, colorize: bool) -> Self {
                self.colorize = Some(colorize);
                self
            }
        }

        impl<S: AsRef<str>> Display for $name<S> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                if self.colorize.unwrap_or_else(probe_rs_color_enabled) {
                    let $var = self.value.as_ref();
                    write!(f, "{}", $style)
                } else {
                    f.write_str(self.value.as_ref())
                }
            }
        }
    };
}

styled!(StackTraceFunction(name) => name.bold().cyan());
styled!(StackTraceAddress(addr) => addr.yellow());
styled!(StackTraceInlineMarker(marker) => marker.italic().dark_yellow());
styled!(StackTraceSourceLocation(loc) => loc.dim().grey());
styled!(Prompt(prompt) => prompt.bold().dark_green());

// The DAP client renders these, so the escape sequences are written directly.
// crossterm suppresses colors when the *server* terminal cannot show them.
styled!(ReplCommandName(name) => format_args!("\x1b[1m{name}\x1b[0m"));
styled!(ReplSymbol(name) => format_args!("\x1b[36m{name}\x1b[0m"));
styled!(ReplAddress(addr) => format_args!("\x1b[33m{addr}\x1b[0m"));
styled!(ReplDim(text) => format_args!("\x1b[2m{text}\x1b[0m"));

/// Format a source location as `path[:line[:column]]`.
pub fn format_source_location(location: &SourceLocation) -> String {
    let mut source = format!("{}", location.path.to_path().display());
    if let Some(line) = location.line {
        source.push_str(&format!(":{line}"));
        if let Some(ColumnType::Column(column)) = location.column {
            source.push_str(&format!(":{column}"));
        }
    }
    source
}

/// Format a target address, and the source location that belongs to it, for the
/// client console.
pub fn format_location(
    address: u64,
    source_location: Option<&SourceLocation>,
    colorize: bool,
) -> String {
    let mut location = format!(
        "{}",
        ReplAddress::new(format!("{address:#010x}")).colorize(colorize)
    );
    if let Some(source) = source_location {
        location.push_str(&format!(
            " {}",
            ReplDim::new(format!("({})", format_source_location(source))).colorize(colorize)
        ));
    }
    location
}

#[cfg(test)]
mod test {
    use typed_path::TypedPath;

    use super::*;

    fn source_location() -> SourceLocation {
        SourceLocation {
            path: TypedPath::derive("/src/main.rs").to_path_buf(),
            line: Some(42),
            column: Some(ColumnType::Column(7)),
            address: Some(0x2000),
        }
    }

    #[test]
    fn formats_an_address_with_its_source_location() {
        pretty_assertions::assert_eq!(
            format_location(0x2000, Some(&source_location()), false),
            "0x00002000 (/src/main.rs:42:7)"
        );
        pretty_assertions::assert_eq!(format_location(0x2000, None, false), "0x00002000");
    }

    #[test]
    fn styles_the_address_and_the_source_location_for_ansi_clients() {
        pretty_assertions::assert_eq!(
            format_location(0x2000, Some(&source_location()), true),
            "\u{1b}[33m0x00002000\u{1b}[0m \u{1b}[2m(/src/main.rs:42:7)\u{1b}[0m"
        );
    }
}
