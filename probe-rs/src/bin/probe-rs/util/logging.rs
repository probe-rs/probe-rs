use indicatif::ProgressBar;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    path::Path,
    sync::{Arc, RwLock},
};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    fmt::format::FmtSpan, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer,
};

/// Stores the progress bar for the logging facility.
static PROGRESS_BAR: Lazy<RwLock<Option<Arc<ProgressBar>>>> = Lazy::new(|| RwLock::new(None));

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

pub fn setup_logging(
    log_path: Option<&Path>,
    default: Option<LevelFilter>,
) -> anyhow::Result<Option<FileLoggerGuard<'_>>> {
    // TODO: we need out own layer to play nice with indicatif
    let stdout_subscriber = tracing_subscriber::fmt::layer()
        .compact()
        .without_time()
        .with_filter(
            EnvFilter::builder()
                .with_default_directive(default.unwrap_or(LevelFilter::Error).into_tracing().into())
                .from_env_lossy(),
        );

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
            Some(pb) if !pb.is_finished() => pb.println(message.as_ref()),
            _ => eprintln!("{}", message.as_ref()),
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
            Some(pb) if !pb.is_finished() => pb.println(message.as_ref()),
            _ => println!("{}", message.as_ref()),
        }
    } else {
        println!("{}", message.as_ref());
    }
}
