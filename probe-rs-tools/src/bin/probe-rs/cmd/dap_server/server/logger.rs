use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::{dap::adapter::DebugAdapter, protocol::ProtocolAdapter},
};
use parking_lot::{Mutex, MutexGuard};
use std::{
    fs::File,
    io::{Write, stderr},
    path::Path,
    sync::Arc,
};

use tracing::{level_filters::LevelFilter, subscriber::DefaultGuard};
use tracing_subscriber::{
    EnvFilter, Layer,
    fmt::{MakeWriter, format::FmtSpan},
    prelude::__tracing_subscriber_SubscriberExt,
    util::SubscriberInitExt,
};

/// DebugLogger manages the temporary file that is used to store the tracing messages that are generated during the DAP sessions.
/// For portions of the Debugger lifetime where no DAP session is active, the tracing messages are sent to `stderr`.
#[derive(Clone)]
pub(crate) struct DebugLogger {
    /// This is a temporary buffer that is periodically flushed.
    ///  - When the DAP server is running, the tracing messages are sent to the console.
    ///  - When the DAP server exits, the remaining messages in this buffer are sent to `stderr`.
    buffer: Arc<Mutex<Vec<u8>>>,
    /// We need to hold onto the tracing `DefaultGuard` for the `lifetime of DebugLogger`.
    /// Using the `DefaultGuard` will ensure that the tracing subscriber be dropped when the `DebugLogger`
    /// is dropped at the end of the `Debugger` lifetime. If we don't set it up this way,
    /// the tests will fail because a default subscriber is already set.
    log_default_guard: Arc<Option<DefaultGuard>>,
}

#[derive(Clone)]
struct WriterWrapper(Arc<Mutex<Vec<u8>>>);

/// Get a handle to the buffered log file, so that `tracing` can write to it.
///
impl MakeWriter<'_> for WriterWrapper {
    type Writer = Self;

    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}

impl Write for WriterWrapper {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut locked_log = self.0.lock();
        locked_log.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut locked_log = self.0.lock();
        locked_log.flush()
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        let mut locked_log = self.0.lock();
        locked_log.write_all(buf)
    }

    fn write_fmt(&mut self, fmt: std::fmt::Arguments<'_>) -> std::io::Result<()> {
        let mut locked_log = self.0.lock();
        locked_log.write_fmt(fmt)
    }
}

impl DebugLogger {
    /// Create a new DebugTraceFile instance
    pub(crate) fn new(log_file: Option<&Path>) -> Result<Self, DebuggerError> {
        let mut debug_logger = Self {
            buffer: Arc::new(Mutex::new(Vec::new())),
            log_default_guard: Arc::new(None),
        };
        debug_logger.log_default_guard = Arc::new(Some(debug_logger.setup_logging(log_file)?));

        Ok(debug_logger)
    }

    fn locked_buffer(&self) -> MutexGuard<Vec<u8>> {
        self.buffer.lock()
    }

    fn process_new_log_lines(&self, mut callback: impl FnMut(&str)) -> Result<(), DebuggerError> {
        let new = {
            let mut locked_log = self.buffer.lock();
            let new_bytes = std::mem::take(&mut *locked_log);

            String::from_utf8_lossy(&new_bytes).to_string()
        };

        let buffer_lines = new.lines();
        for next_line in buffer_lines {
            callback(next_line);
        }

        Ok(())
    }

    /// Flush the buffer to the DAP client's Debug Console
    pub(crate) fn flush_to_dap(
        &self,
        debug_adapter: &mut DebugAdapter<impl ProtocolAdapter>,
    ) -> Result<(), DebuggerError> {
        self.process_new_log_lines(|line| {
            debug_adapter.log_to_console(line);
        })
    }

    /// Flush the buffer to the stderr
    pub(crate) fn flush(&self) -> Result<(), DebuggerError> {
        self.process_new_log_lines(|line| eprintln!("{}", line))
    }

    /// Setup logging, according to the following rules.
    /// 1. If the RUST_LOG environment variable is set, use it as a `LevelFilter` to configure a subscriber that
    ///     logs to the given destination, or default to `RUST_LOG=probe_rs_debug=warn`
    /// 2. If no `log_file` destination is supplied, output will be written to the DAP client's Debug Console,
    /// 3. Irrespective of the RUST_LOG environment variable, configure a subscriber that will write with `LevelFilter::ERROR` to stderr,
    ///     because these errors are picked up and reported to the user by the VSCode extension, when no DAP session is available.
    pub fn setup_logging(
        &mut self,
        log_file: Option<&Path>,
    ) -> Result<DefaultGuard, DebuggerError> {
        let environment_filter = if std::env::var("RUST_LOG").is_ok() {
            EnvFilter::from_default_env()
        } else {
            EnvFilter::new("probe_rs=warn")
        };

        match log_file {
            Some(log_path) => {
                let log_file = File::create(log_path)?;
                let log_message = format!(
                    r#"Log output for "{environment_filter}" will be written to: {}"#,
                    log_path.display()
                );

                // Subscriber for the designated log file.
                let file_subscriber = tracing_subscriber::fmt::layer()
                    .json()
                    .with_file(true)
                    .with_line_number(true)
                    .with_span_events(FmtSpan::FULL)
                    .with_writer(log_file)
                    .with_filter(environment_filter);

                // We need to always log errors to stderr, so that the DAP extension can monitor for them.
                let stderr_subscriber = tracing_subscriber::fmt::layer()
                    .compact()
                    .with_ansi(false)
                    .with_line_number(true)
                    .with_span_events(FmtSpan::FULL)
                    .with_writer(stderr)
                    .with_filter(LevelFilter::ERROR);

                // The stderr subscriber will always log errors to stderr, so that the VSCode extension can monitor for them.
                let log_default_guard = tracing_subscriber::registry()
                    .with(stderr_subscriber)
                    .with(file_subscriber)
                    .set_default();
                // Tell the user where RUST_LOG messages are written.
                self.log_to_console(&log_message)?;
                Ok(log_default_guard)
            }
            None => {
                if let Some(LevelFilter::TRACE) = environment_filter.max_level_hint() {
                    return Err(DebuggerError::UserMessage(String::from(
                        r#"Using the `TRACE` log level to stream data to the console may have adverse effects on performance.
                        Consider using a less verbose log level, or use one of the `logFile` or `logToDir` options."#,
                    )));
                }

                let log_message = format!(
                    r#"Log output for "{environment_filter}" will be written to the Debug Console."#
                );

                // If no log file desitination is specified, send logs via the buffer, to the DAP
                // client's Debug Console.
                let buffer_layer = tracing_subscriber::fmt::layer()
                    .compact()
                    .with_ansi(false)
                    .without_time()
                    .with_line_number(true)
                    .with_span_events(FmtSpan::FULL)
                    .with_writer(WriterWrapper(self.buffer.clone()))
                    .with_filter(environment_filter);

                let log_default_guard = tracing_subscriber::registry()
                    .with(buffer_layer)
                    .set_default();
                // Tell the user where RUST_LOG messages are written.
                self.log_to_console(&log_message)?;
                Ok(log_default_guard)
            }
        }
    }

    /// We can send messages directly to the console, irrespective of log levels, by writing to the `buffer_file`.
    /// If no `buffer_file` is available, we write to `stderr`.
    pub(crate) fn log_to_console(&mut self, message: &str) -> Result<(), DebuggerError> {
        let mut locked_log = self.locked_buffer();

        writeln!(locked_log, "probe-rs-debug: {message}")?;

        Ok(())
    }
}
