//! Named text styles for the CLI and for the DAP REPL.

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
