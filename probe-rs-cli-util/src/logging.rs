use colored::*;
use env_logger::{Builder, Logger};
use indicatif::ProgressBar;
use log::{Level, LevelFilter, Log, Record};
use once_cell::sync::Lazy;
#[cfg(feature = "sentry")]
use sentry::{
    integrations::panic::PanicIntegration,
    types::{Dsn, Uuid},
    Breadcrumb,
};
use simplelog::{CombinedLogger, SharedLogger};
#[cfg(feature = "sentry")]
use std::{borrow::Cow, error::Error, panic::PanicInfo, str::FromStr};
use std::{
    fmt,
    io::Write,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, RwLock,
    },
};
use terminal_size::{Height, Width};

/// The maximum window width of the terminal, given in characters possible.
static MAX_WINDOW_WIDTH: AtomicUsize = AtomicUsize::new(0);

/// Stores the progress bar for the logging facility.
static PROGRESS_BAR: Lazy<RwLock<Option<Arc<ProgressBar>>>> = Lazy::new(|| RwLock::new(None));

#[cfg(feature = "sentry")]
static LOG: Lazy<Arc<RwLock<Vec<Breadcrumb>>>> = Lazy::new(|| Arc::new(RwLock::new(vec![])));

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

struct ShareableLogger(Logger);

impl Log for ShareableLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= self.0.filter()
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            self.0.log(record);
        }
    }

    fn flush(&self) {
        self.0.flush();
    }
}

impl SharedLogger for ShareableLogger {
    fn level(&self) -> LevelFilter {
        self.0.filter()
    }

    fn config(&self) -> Option<&simplelog::Config> {
        None
    }

    fn as_log(self: Box<Self>) -> Box<dyn log::Log> {
        Box::new(*self)
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

        let guard = PROGRESS_BAR.write().unwrap();
        if let Some(pb) = &*guard {
            pb.println(format!("       {} {} > {}", level, target, record.args()));
        } else {
            println!("       {} {} > {}", level, target, record.args());
        }

        Ok(())
    });

    // Sentry logging (all log levels except tracing (to not clog the server disk & internet sink)).
    #[cfg(feature = "sentry")]
    let mut sentry = {
        let mut sentry = Builder::new();

        // Always use the Debug log level.
        sentry.filter_level(LevelFilter::Debug);

        // Define our custom log format.
        sentry.format(move |_f, record| {
            let mut log_guard = LOG.write().unwrap();
            log_guard.push(Breadcrumb {
                level: match record.level() {
                    Level::Error => sentry::Level::Error,
                    Level::Warn => sentry::Level::Warning,
                    Level::Info => sentry::Level::Info,
                    Level::Debug => sentry::Level::Debug,
                    // This mapping is intended as unfortunately, Sentry does not have any trace level for events & breadcrumbs.
                    Level::Trace => sentry::Level::Debug,
                },
                category: Some(record.target().to_string()),
                message: Some(format!("{}", record.args())),
                ..Default::default()
            });

            Ok(())
        });

        sentry
    };

    CombinedLogger::init(vec![
        Box::new(ShareableLogger(log_builder.build())),
        #[cfg(feature = "sentry")]
        Box::new(ShareableLogger(sentry.build())),
    ])
    .unwrap();
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

#[cfg(feature = "sentry")]
fn send_logs() {
    let mut log_guard = LOG.write().unwrap();

    for breadcrumb in log_guard.drain(..) {
        sentry::add_breadcrumb(breadcrumb);
    }
}

#[cfg(feature = "sentry")]
fn sentry_config(release: String) -> sentry::ClientOptions {
    sentry::ClientOptions {
        dsn: Some(
            Dsn::from_str(
                "https://820ae3cb7b524b59af68d652aeb8ac3a@o473674.ingest.sentry.io/5508777",
            )
            .unwrap(),
        ),
        release: Some(Cow::<'static>::Owned(release)),
        #[cfg(debug_assertions)]
        environment: Some(Cow::Borrowed("Development")),
        #[cfg(not(debug_assertions))]
        environment: Some(Cow::Borrowed("Production")),
        default_integrations: false,
        ..Default::default()
    }
}

#[derive(Clone, Debug)]
pub struct Metadata {
    pub chip: Option<String>,
    pub probe: Option<String>,
    pub speed: Option<String>,
    pub release: String,
    pub commit: String,
}

#[cfg(feature = "sentry")]
/// Sets the metadata concerning the current probe-rs session on the sentry scope.
fn set_metadata(metadata: &Metadata) {
    sentry::configure_scope(|scope| {
        if let Some(chip) = metadata.chip.as_ref() {
            scope.set_tag("chip", chip);
        }
        if let Some(probe) = metadata.probe.as_ref() {
            scope.set_tag("probe", probe);
        }
        if let Some(speed) = metadata.speed.as_ref() {
            scope.set_tag("speed", speed);
        }
        scope.set_tag("commit", &metadata.commit);
    })
}

#[cfg(feature = "sentry")]
const SENTRY_SUCCESS: &str = r"Your error was reported successfully. If you don't mind, please open an issue on Github and include the UUID:";

#[cfg(feature = "sentry")]
fn print_uuid(uuid: Uuid) {
    let size = terminal_size::terminal_size();
    if let Some((Width(w), Height(_h))) = size {
        let lines = chunk_string(&format!("{} {}", SENTRY_SUCCESS, uuid), w as usize - 14);

        for (i, l) in lines.iter().enumerate() {
            if i == 0 {
                println!("  {} {}", "Thank You!".cyan().bold(), l);
            } else {
                println!("             {}", l);
            }
        }
    } else {
        print!("{}", SENTRY_HINT);
    }
}

#[cfg(feature = "sentry")]
/// Captures an std::error::Error with sentry and sends all previously captured logs.
pub fn capture_error<E>(metadata: &Metadata, error: &E)
where
    E: Error + ?Sized,
{
    let _guard = sentry::init(sentry_config(metadata.release.clone()));
    set_metadata(metadata);
    send_logs();
    let uuid = sentry::capture_error(error);
    print_uuid(uuid);
}

#[cfg(feature = "sentry")]
/// Captures an anyhow error with sentry and sends all previously captured logs.
pub fn capture_anyhow(metadata: &Metadata, error: &anyhow::Error) {
    let _guard = sentry::init(sentry_config(metadata.release.clone()));
    set_metadata(metadata);
    send_logs();
    let uuid = sentry::integrations::anyhow::capture_anyhow(error);
    print_uuid(uuid);
}

#[cfg(feature = "sentry")]
/// Captures a panic with sentry and sends all previously captured logs.
pub fn capture_panic(metadata: &Metadata, info: &PanicInfo<'_>) {
    let _guard = sentry::init(sentry_config(metadata.release.clone()));
    set_metadata(metadata);
    send_logs();
    let uuid = sentry::capture_event(PanicIntegration::new().event_from_panic_info(info));
    print_uuid(uuid);
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

const SENTRY_HINT: &str = r"Unfortunately probe-rs encountered an unhandled problem. To help the devs, you can automatically log the error to sentry.io. Your data will be transmitted completely anonymously and cannot be associated with you directly. To hide this message in the future, please set $PROBE_RS_SENTRY to 'true' or 'false'. Do you wish to transmit the data? Y/n: ";

/// Chunks the given string into pieces of maximum_length whilst honoring word boundaries.
fn chunk_string(s: &str, max_width: usize) -> Vec<String> {
    let string = s.chars().collect::<Vec<char>>();

    let mut result = vec![];

    let mut last_ws = 0;
    let mut offset = 0;
    let mut i = 0;
    let mut t_max_width = max_width;
    while i < string.len() {
        let c = string[i];
        if c.is_whitespace() {
            last_ws = i;
        }
        if i > offset + t_max_width {
            if last_ws > offset {
                let s = string[offset..last_ws].iter().collect::<String>();
                result.push(s);
                t_max_width = max_width;
            } else {
                t_max_width += 1;
            }

            offset = last_ws + 1;
            i = last_ws + 1;
        } else {
            i += 1;
        }
    }
    result.push(string[offset..].iter().collect::<String>());
    result
}

/// Displays the text to ask if the crash should be reported.
pub fn ask_to_log_crash() -> bool {
    if let Ok(var) = std::env::var("PROBE_RS_SENTRY") {
        var == "true"
    } else {
        let size = terminal_size::terminal_size();
        if let Some((Width(w), Height(_h))) = size {
            let lines = chunk_string(SENTRY_HINT, w as usize - 14);

            for (i, l) in lines.iter().enumerate() {
                if i == 0 {
                    println!("        {} {}", "Hint".blue().bold(), l);
                } else if i == lines.len() - 1 {
                    print!("             {}", l);
                } else {
                    println!("             {}", l);
                }
            }
        } else {
            print!("{}", SENTRY_HINT);
        }

        std::io::stdout().flush().ok();
        let result = if let Ok(s) = text() {
            let s = s.to_lowercase();
            "yes".starts_with(&s)
        } else {
            false
        };

        println!();

        result
    }
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
