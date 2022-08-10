//! Pretty printing the backtrace

use std::{
    borrow::Cow,
    fmt::Write,
    io::{self, Write as _},
};

use colored::Colorize as _;

use crate::dep;

use super::{symbolicate::Frame, Settings};

/// Pretty prints processed backtrace frames up to `backtrace_limit`
pub fn backtrace(frames: &[Frame], settings: &Settings) -> io::Result<()> {
    let mut stderr = io::stderr().lock();
    writeln!(stderr, "{}", "stack backtrace:".dimmed())?;

    let mut frame_index = 0;
    for frame in frames {
        match frame {
            Frame::Exception => writeln!(stderr, "      <exception entry>")?,
            Frame::Subroutine(subroutine) => {
                let is_local_function = subroutine
                    .location
                    .as_ref()
                    .map(|location| location.path_is_relative)
                    .unwrap_or(false);

                let mut line = format!("{:>4}:", frame_index);
                if settings.include_addresses || subroutine.name.is_none() {
                    write!(line, " {:#010x} @", subroutine.pc).unwrap();
                }
                write!(
                    line,
                    " {}",
                    subroutine.name.as_deref().unwrap_or("<unknown>")
                )
                .unwrap();

                let colorized_line = if is_local_function {
                    line.bold()
                } else {
                    line.normal()
                };
                writeln!(stderr, "{}", colorized_line)?;

                if let Some(location) = &subroutine.location {
                    let dep_path = dep::Path::from_std_path(&location.path);

                    let path = if settings.shorten_paths {
                        dep_path.format_short()
                    } else {
                        dep_path.format_highlight()
                    };

                    let line = location.line;
                    let column = location
                        .column
                        .map(|column| Cow::Owned(format!(":{}", column)))
                        .unwrap_or(Cow::Borrowed(""));

                    writeln!(stderr, "        at {}:{}{}", path, line, column)?;
                }

                frame_index += 1;

                if frame_index >= settings.backtrace_limit {
                    log::warn!(
                        "maximum backtrace length of {} reached; cutting off the rest.",
                        settings.backtrace_limit
                    );
                    log::warn!("note: re-run with `--max-backtrace-len=<your maximum>` to extend this limit");

                    break;
                }
            }
        }
    }

    Ok(())
}
