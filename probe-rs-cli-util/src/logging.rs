use colored::*;
use env_logger::Builder;
use indicatif::ProgressBar;
use log::{Level, LevelFilter};
use sentry::internals::Dsn;
use std::{
    borrow::Cow,
    error::Error,
    fmt,
    panic::PanicInfo,
    str::FromStr,
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
    static ref LOG: Arc<RwLock<Vec<LogEntry>>> = Arc::new(RwLock::new(vec![]));
}

struct LogEntry {
    level: Level,
    message: String,
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

        let mut log_guard = LOG.write().unwrap();
        log_guard.push(LogEntry {
            level: record.level(),
            message: format!("{} > {}", target, record.args()),
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

fn send_logs() {
    let mut log_guard = LOG.write().unwrap();

    for log in log_guard.drain(..) {
        sentry::capture_message(
            &log.message,
            match log.level {
                Level::Error => sentry::Level::Error,
                Level::Warn => sentry::Level::Warning,
                Level::Info => sentry::Level::Info,
                Level::Debug => sentry::Level::Debug,
                Level::Trace => sentry::Level::Debug,
            },
        );
    }
}

fn sentry_config(release: String) -> sentry::ClientOptions {
    sentry::ClientOptions {
        dsn: Some(
            Dsn::from_str("https://4396a23b463a46b8b3bfa883910333fe@sentry.technokrat.ch/7")
                .unwrap(),
        ),
        release: Some(Cow::<'static>::Owned(release.to_string())),
        #[cfg(debug_assertions)]
        environment: Some(Cow::Borrowed("Development")),
        #[cfg(not(debug_assertions))]
        environment: Some(Cow::Borrowed("Production")),
        ..Default::default()
    }
}

pub struct Metadata {
    chip: Option<String>,
    probe: Option<String>,
    release: String,
}

/// Sets the metadata concerning the current probe-rs session on the sentry scope.
fn set_metadata(metadata: Metadata) {
    sentry::configure_scope(|scope| {
        metadata.chip.map(|chip| scope.set_tag("chip", chip));
        metadata.probe.map(|probe| scope.set_tag("probe", probe));
    })
}

/// Captures an std::error::Error with sentry and sends all previously captured logs.
pub fn capture_error<E>(metadata: Metadata, error: &E)
where
    E: Error + ?Sized,
{
    let _guard = sentry::init(sentry_config(metadata.release.clone()));
    set_metadata(metadata);
    send_logs();
    sentry::capture_error(error);
}

/// Captures an anyhow error with sentry and sends all previously captured logs.
pub fn capture_anyhow(metadata: Metadata, error: &anyhow::Error) {
    let _guard = sentry::init(sentry_config(metadata.release.clone()));
    set_metadata(metadata);
    send_logs();
    sentry::integrations::anyhow::capture_anyhow(error);
}

/// Captures a panic with sentry and sends all previously captured logs.
pub fn capture_panic(metadata: Metadata, info: &PanicInfo<'_>) {
    let _guard = sentry::init(sentry_config(metadata.release));
    send_logs();
    sentry::integrations::panic::panic_handler(info);
}

/// Ask for a line of text.
fn text() -> std::io::Result<String> {
    // Read up to the first newline or EOF.

    let mut out = String::new();
    std::io::stdin().read_line(&mut out)?;

    // Only capture up to the first newline.
    if let Some(mut newline) = out.find('\n') {
        if newline > 0 && out.as_bytes()[newline - 1] == b'\r' {
            newline -= 1;
        }
        out.truncate(newline);
    }

    Ok(out)
}

/// Displays the text to ask if the crash should be reported.
pub fn ask_to_log_crash() -> bool {
    if let Ok(var) = std::env::var("PROBE_RS_SENTRY") {
        var == "true"
    } else {
        println(format!(
            "        {} {}",
            "Hint".blue().bold(),
            "Unfortunately probe-rs encountered an unhandled problem. To help the devs, you can automatically log the error to sentry.technokrat.ch."
        ));
        println(format!(
            "             {}",
            "Your data will be transmitted completely anonymous and cannot be associated with you directly."
        ));
        println(format!(
            "             {}",
            "To Hide this message in the future, please set $PROBE_RS_SENTRY to 'true' or 'false'."
        ));
        println(format!(
            "             {}",
            "Do you wish to transmit the data? y/N"
        ));
        if let Ok(s) = text() {
            let s = s.to_lowercase();
            if s.is_empty() {
                false
            } else if "yes".starts_with(&s) {
                true
            } else if "no".starts_with(&s) {
                false
            } else {
                false
            }
        } else {
            false
        }
    }
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
