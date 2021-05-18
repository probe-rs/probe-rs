//! Pretty printing the backtrace

use std::borrow::Cow;

use colored::Colorize as _;

use crate::utils;

use super::{symbolicate::Frame, Settings};

/// Pretty prints processed backtrace frames up to `max_backtrace_len`
pub(crate) fn backtrace(frames: &[Frame], settings: &Settings) {
    println!("{}", "stack backtrace:".dimmed());

    let mut frame_index = 0;
    for frame in frames {
        match frame {
            Frame::Exception => {
                println!("      <exception entry>");
            }

            Frame::Subroutine(subroutine) => {
                let name = match &subroutine.name_or_pc {
                    either::Either::Left(name) => Cow::Borrowed(name),
                    either::Either::Right(pc) => Cow::Owned(format!("??? (PC={:#010x})", pc)),
                };

                let is_local_function = subroutine
                    .location
                    .as_ref()
                    .map(|location| location.path_is_relative)
                    .unwrap_or(false);

                let line = format!("{:>4}: {}", frame_index, name);
                let colorized_line = if is_local_function {
                    line.bold()
                } else {
                    line.normal()
                };
                println!("{}", colorized_line);

                if let Some(location) = &subroutine.location {
                    let path = if settings.compress_cratesio_dep_paths {
                        utils::compress_cratesio_dep_path(&location.path)
                    } else {
                        location.path.display().to_string()
                    };
                    let line = location.line;
                    let column = location
                        .column
                        .map(|column| Cow::Owned(format!(":{}", column)))
                        .unwrap_or(Cow::Borrowed(""));

                    let line = format!("        at {}:{}{}", path, line, column);
                    let colorized_line = if is_local_function {
                        line.normal()
                    } else {
                        line.dimmed()
                    };
                    println!("{}", colorized_line);
                }

                frame_index += 1;

                if frame_index >= settings.max_backtrace_len {
                    log::warn!(
                        "maximum backtrace length of {} reached; cutting off the rest.const ",
                        settings.max_backtrace_len
                    );
                    log::warn!("note: re-run with `--max-backtrace-len=<your maximum>` to extend this limit");

                    break;
                }
            }
        }
    }
}
