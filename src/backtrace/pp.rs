//! Pretty printing the backtrace

use std::{borrow::Cow, path::Path};

use colored::Colorize as _;

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
                        compress_cratesio_dep_path(&location.path)
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

// TODO use this for defmt logs
fn compress_cratesio_dep_path(path: &Path) -> String {
    if let Some(dep) = Dependency::from_path(path) {
        format!("[{}]/{}", dep.name_version, dep.path.display())
    } else {
        path.display().to_string()
    }
}

struct Dependency<'p> {
    name_version: &'p str,
    path: &'p Path,
}

impl<'p> Dependency<'p> {
    // as of Rust 1.52.1 this path looks like this on Linux
    // /home/some-user/.cargo/registry/src/github.com-0123456789abcdef/crate-name-0.1.2/src/lib.rs
    // on Windows the `/home/some-user` part becomes something else
    fn from_path(path: &'p Path) -> Option<Self> {
        if !path.is_absolute() {
            return None;
        }

        let mut components = path.components();
        let _registry = components.find(|component| match component {
            std::path::Component::Normal(component) => *component == "registry",
            _ => false,
        })?;

        if let std::path::Component::Normal(src) = components.next()? {
            if src != "src" {
                return None;
            }
        }

        if let std::path::Component::Normal(github) = components.next()? {
            let github = github.to_str()?;
            if !github.starts_with("github.com-") {
                return None;
            }
        }

        if let std::path::Component::Normal(name_version) = components.next()? {
            let name_version = name_version.to_str()?;
            Some(Dependency {
                name_version,
                path: components.as_path(),
            })
        } else {
            None
        }
    }
}
