use indicatif::MultiProgress;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{fs::File, path::Path, sync::LazyLock};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    EnvFilter, Layer, fmt::format::FmtSpan, layer::SubscriberExt, util::SubscriberInitExt,
};

/// Stores the progress bar for the logging facility.
static PROGRESS_BAR: LazyLock<Mutex<Option<MultiProgress>>> = LazyLock::new(|| Mutex::new(None));

pub struct FileLoggerGuard<'a> {
    _append_guard: WorkerGuard,
    log_path: &'a Path,
}

impl<'a> FileLoggerGuard<'a> {
    fn new(_append_guard: WorkerGuard, log_path: &'a Path) -> Self {
        // Log after initializing the logger, so we can see the log path.
        tracing::info!("Writing log to {:?}", log_path);

        Self {
            _append_guard,
            log_path,
        }
    }
}

impl Drop for FileLoggerGuard<'_> {
    fn drop(&mut self) {
        // TODO: drop
        tracing::info!("Wrote log to {:?}", self.log_path);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[clap(rename_all = "UPPER")]
#[serde(rename_all = "UPPERCASE")]
pub enum LevelFilter {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LevelFilter {
    fn into_tracing(self) -> tracing::level_filters::LevelFilter {
        match self {
            Self::Off => tracing::level_filters::LevelFilter::OFF,
            Self::Error => tracing::level_filters::LevelFilter::ERROR,
            Self::Warn => tracing::level_filters::LevelFilter::WARN,
            Self::Info => tracing::level_filters::LevelFilter::INFO,
            Self::Debug => tracing::level_filters::LevelFilter::DEBUG,
            Self::Trace => tracing::level_filters::LevelFilter::TRACE,
        }
    }
}

// A custom writer that delegates to indicatif's printing when available.
struct ProgressBarWriter;

impl std::io::Write for ProgressBarWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let out_str = String::from_utf8_lossy(buf);

        // Extra line endings look wrong, so strip them. We can't just trim all, because that would
        // also remove intentional newlines, too.
        let out_str = if let Some(str) = out_str.strip_suffix("\r\n") {
            str
        } else if let Some(str) = out_str.strip_suffix('\n') {
            str
        } else {
            out_str.as_ref()
        };

        eprintln(out_str);

        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Configures tracing and sets up the logging facility.
///
/// # Arguments
///
/// * `log_path` - The path to the log file. If `None`, log messages will not be stored in a file.
/// * `default` - The default log level to use. If `None`, falls back to `RUST_LOG` in the environment.
pub fn setup_logging(
    log_path: Option<&Path>,
    default: Option<LevelFilter>,
) -> anyhow::Result<Option<FileLoggerGuard<'_>>> {
    let stdout_subscriber = tracing_subscriber::fmt::layer()
        .compact()
        .without_time()
        .with_writer(|| ProgressBarWriter)
        .with_filter(match default {
            Some(filter) => {
                // We have a default (from config or command argument), ignore RUST_LOG.
                EnvFilter::builder()
                    .with_default_directive(filter.into_tracing().into())
                    .parse_lossy("")
            }
            None => {
                // No default, use RUST_LOG or fall back to WARN.
                EnvFilter::builder()
                    .with_default_directive(tracing::level_filters::LevelFilter::WARN.into())
                    .from_env_lossy()
            }
        });

    let Some(log_path) = log_path else {
        tracing_subscriber::registry()
            .with(stdout_subscriber)
            .init();

        return Ok(None);
    };

    let log_file = File::create(log_path)?;

    let (file_appender, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .lossy(false)
        .buffered_lines_limit(128 * 1024)
        .finish(log_file);

    let file_subscriber = tracing_subscriber::fmt::layer()
        .json()
        .with_file(true)
        .with_line_number(true)
        .with_span_events(FmtSpan::FULL)
        .with_writer(file_appender);

    tracing_subscriber::registry()
        .with(stdout_subscriber)
        .with(file_subscriber)
        .init();

    Ok(Some(FileLoggerGuard::new(guard, log_path)))
}

/// Sets the currently displayed progress bar of the CLI.
pub fn set_progress_bar(progress: MultiProgress) {
    *PROGRESS_BAR.lock() = Some(progress);
}

/// Disables the currently displayed progress bar of the CLI.
pub fn clear_progress_bar() {
    *PROGRESS_BAR.lock() = None;
}

/// Writes an error to stderr.
/// This function respects the progress bars of the CLI that might be displayed and displays the message above it if any are.
pub fn eprintln(message: impl AsRef<str>) {
    fn inner(message: &str) {
        let locked = PROGRESS_BAR.lock();
        match locked.as_ref() {
            Some(pb) => {
                let _ = pb.println(message);
            }
            None => eprintln!("{message}"),
        }
    }
    inner(message.as_ref())
}

/// Writes a message to stdout with a newline at the end.
/// This function respects the progress bars of the CLI that might be displayed and displays the message above it if any are.
pub fn println(message: impl AsRef<str>) {
    fn inner(message: &str) {
        let locked = PROGRESS_BAR.lock();
        match locked.as_ref() {
            Some(pb) => {
                let _ = pb.println(message);
            }
            None => println!("{message}"),
        }
    }
    inner(message.as_ref())
}
