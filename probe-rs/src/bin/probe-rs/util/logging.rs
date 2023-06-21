use colored::*;
use indicatif::ProgressBar;
use is_terminal::IsTerminal;
use log::{Level, LevelFilter, Log, Record};
use once_cell::sync::Lazy;
use pretty_env_logger::env_logger::{Builder, Logger};
use std::{
    fmt::{self},
    io::Write,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, RwLock,
    },
};

/// The maximum window width of the terminal, given in characters possible.
static MAX_WINDOW_WIDTH: AtomicUsize = AtomicUsize::new(0);

/// Stores the progress bar for the logging facility.
static PROGRESS_BAR: Lazy<RwLock<Option<Arc<ProgressBar>>>> = Lazy::new(|| RwLock::new(None));

/// A structure to hold a string with a padding attached to the start of it.
struct Padded<T> {
    value: T,
    width: usize,
}

impl<T: fmt::Display> fmt::Display for Padded<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{: <width$}", self.value, width = self.width)
    }
}

/// Get the maximum between the window width and the length of the given string.
fn max_target_width(target: &str) -> usize {
    let max_width = MAX_WINDOW_WIDTH.load(Ordering::Relaxed);
    if max_width < target.len() {
        MAX_WINDOW_WIDTH.store(target.len(), Ordering::Relaxed);
        target.len()
    } else {
        max_width
    }
}

/// Helper to receive a color for a given level.
fn colored_level(level: Level) -> ColoredString {
    match level {
        Level::Trace => "TRACE".magenta().bold(),
        Level::Debug => "DEBUG".blue().bold(),
        Level::Info => " INFO".green().bold(),
        Level::Warn => " WARN".yellow().bold(),
        Level::Error => "ERROR".red().bold(),
    }
}

/// Logger wrapper that can coexist peacefully with indicatif progressbars.
struct CliLogger {
    env_logger: Logger,
    output_is_terminal: bool,
}

impl Log for CliLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= self.env_logger.filter()
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            // If the output is not an interactive terminal,
            // indicatif will not display anything, so messages
            // forwared to it would be swallowed.
            if !self.output_is_terminal {
                self.env_logger.log(record);
            } else {
                let guard = PROGRESS_BAR.write().unwrap();

                // Print the log message above the progress bar, if one is present.
                if let Some(pb) = &*guard {
                    let target = record.target();
                    let max_width = max_target_width(target);

                    let level = colored_level(record.level());

                    let target = Padded {
                        value: target.bold(),
                        width: max_width,
                    };

                    pb.println(format!("       {} {} > {}", level, target, record.args()));
                } else {
                    self.env_logger.log(record);
                }
            }
        }
    }

    fn flush(&self) {
        self.env_logger.flush();
    }
}

/// Initialize the logger.
///
/// There are two sources for log level configuration:
///
/// - The log level value passed to this function
/// - The user can set the `RUST_LOG` env var, which overrides the log level passed to this function.
///
/// The config file only accepts a log level, while the `RUST_LOG` variable
/// supports the full `env_logger` syntax, including filtering by crate and
/// module.
pub fn init(level: Option<Level>) {
    // User visible logging.

    let mut log_builder = Builder::new();

    // First, apply the log level given to this function.
    if let Some(level) = level {
        log_builder.filter_level(level.to_level_filter());
    } else {
        log_builder.filter_level(LevelFilter::Warn);
    }

    // Then override that with the `RUST_LOG` env var, if set.
    if let Ok(s) = ::std::env::var("RUST_LOG") {
        log_builder.parse_filters(&s);
    }

    // Define our custom log format.
    log_builder.format(move |f, record| {
        let target = record.target();
        let max_width = max_target_width(target);

        let level = colored_level(record.level());

        let mut style = f.style();
        let target = style.set_bold(true).value(Padded {
            value: target,
            width: max_width,
        });

        writeln!(f, "       {} {} > {}", level, target, record.args())
    });

    let output_is_terminal = std::io::stderr().is_terminal();

    let logger = Box::new(CliLogger {
        env_logger: log_builder.build(),
        output_is_terminal,
    });
    log::set_max_level(logger.env_logger.filter());
    log::set_boxed_logger(logger).unwrap();
}

/// Sets the currently displayed progress bar of the CLI.
pub fn set_progress_bar(progress: Arc<ProgressBar>) {
    let mut guard = PROGRESS_BAR.write().unwrap();
    *guard = Some(progress);
}

/// Disables the currently displayed progress bar of the CLI.
pub fn clear_progress_bar() {
    let mut guard = PROGRESS_BAR.write().unwrap();
    *guard = None;
}

/// Writes an error to stderr.
/// This function respects the progress bars of the CLI that might be displayed and displays the message above it if any are.
pub fn eprintln(message: impl AsRef<str>) {
    if let Ok(guard) = PROGRESS_BAR.try_write() {
        match guard.as_ref() {
            Some(pb) if !pb.is_finished() => {
                pb.println(message.as_ref());
            }
            _ => {
                eprintln!("{}", message.as_ref());
            }
        }
    } else {
        eprintln!("{}", message.as_ref());
    }
}

/// Writes a message to stdout with a newline at the end.
/// This function respects the progress bars of the CLI that might be displayed and displays the message above it if any are.
pub fn println(message: impl AsRef<str>) {
    if let Ok(guard) = PROGRESS_BAR.try_write() {
        match guard.as_ref() {
            Some(pb) if !pb.is_finished() => {
                pb.println(message.as_ref());
            }
            _ => {
                println!("{}", message.as_ref());
            }
        }
    } else {
        println!("{}", message.as_ref());
    }
}
