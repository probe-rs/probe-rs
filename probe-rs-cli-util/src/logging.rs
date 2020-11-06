use colored::*;
use env_logger::Builder;
use indicatif::ProgressBar;
use log::{Level, LevelFilter};
use std::{
    fmt,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, RwLock,
    },
};

/// The maximum window width of the terminal, given in characters possible.
static MAX_WINDOW_WIDTH: AtomicUsize = AtomicUsize::new(0);

lazy_static::lazy_static! {
    /// Stores the progress bar for the logging facility.
    static ref PROGRESS_BAR: RwLock<Option<Arc<ProgressBar>>> = RwLock::new(None);
}

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
    let mut builder = Builder::new();

    // First, apply the log level given to this function.
    if let Some(level) = level {
        builder.filter_level(level.to_level_filter());
    } else {
        builder.filter_level(LevelFilter::Warn);
    }

    // Then override that with the `RUST_LOG` env var, if set.
    if let Ok(s) = ::std::env::var("RUST_LOG") {
        builder.parse_filters(&s);
    }

    // Define our custom log format.
    builder.format(move |f, record| {
        let target = record.target();
        let max_width = max_target_width(target);

        let level = colored_level(record.level());

        let mut style = f.style();
        let target = style.set_bold(true).value(Padded {
            value: target,
            width: max_width,
        });

        let guard = PROGRESS_BAR.write().unwrap();
        if let Some(pb) = &*guard {
            pb.println(format!("       {} {} > {}", level, target, record.args()));
        } else {
            println!("       {} {} > {}", level, target, record.args());
        }

        Ok(())
    });

    builder.init();
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
    let guard = PROGRESS_BAR.write().unwrap();

    match guard.as_ref() {
        Some(pb) if !pb.is_finished() => {
            pb.println(message.as_ref());
        }
        _ => {
            eprintln!("{}", message.as_ref());
        }
    }
}

/// Writes a message to stdout.
/// This function respects the progress bars of the CLI that might be displayed and displays the message above it if any are.
pub fn println(message: impl AsRef<str>) {
    let guard = PROGRESS_BAR.write().unwrap();
    if let Some(pb) = &*guard {
        pb.println(message.as_ref());
    } else {
        println!("{}", message.as_ref());
    }
}
