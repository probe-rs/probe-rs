use crate::cmd::dap_server::{
    debug_adapter::{dap::adapter::DebugAdapter, protocol::ProtocolAdapter},
    DebuggerError,
};
use anyhow::anyhow;
use std::{
    fs::File,
    io::{stderr, BufRead, BufReader, LineWriter, SeekFrom, Write},
    path::Path,
    sync::Mutex,
};
use tempfile::tempfile;
use tracing::{level_filters::LevelFilter, subscriber::DefaultGuard};
use tracing_subscriber::{
    fmt::{format::FmtSpan, writer::BoxMakeWriter, MakeWriter},
    prelude::__tracing_subscriber_SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter, Layer,
};

/// DebugLogger manages the temporary file that is used to store the tracing messages that are generated during the DAP sessions.
/// For portions of the Debugger lifetime where no DAP session is active, the tracing messages are sent to `stderr`.
pub(crate) struct DebugLogger {
    /// This is a temporary file, created inside of `std::env::temp_dir()`,
    /// and will be automatically removed by the OS when the last handle to it is closed.
    ///  - When the DAP server is running, the tracing messages are sent to the console.
    ///  - When the DAP server exits, the remaining messages in this buffer are sent to `stderr`.
    buffer_file: Mutex<File>,
    /// Keep track of where we are in the file, so that we don't re-read or overwrite data.
    seek_pointer: SeekFrom,
    /// We need to hold onto the tracing `DefaultGuard` for the `lifetime of DebugLogger`.
    /// Using the `DefaultGuard` will ensure that the tracing subscriber be dropped when the `DebugLogger`
    /// is dropped at the end of the `Debugger` lifetime. If we don't set it up this way,
    /// the tests will fail because a default subscriber is already set.
    log_default_guard: Option<DefaultGuard>,
}

/// On the rare condition that the DebugLogger `buffer_file`` is locked while the DAP server is
/// sending data to the client, we can still write to `stderr`. Since this will only happen
/// while a DAP session is active, the VSCode extension will be able to intercept this write to
/// `stderr` and display it to the user in the Console Log, intespersed with the
/// data from the `buffer_file`.
impl MakeWriter<'_> for DebugLogger {
    type Writer = File;

    fn make_writer(&self) -> Self::Writer {
        // The API doesn't allow graceful exit, but we do not expect locking of the Mutex to fail.
        // If it does, we want to know about it.
        #[allow(clippy::expect_used)]
        self.buffer_file_handle()
            .expect("Failed to get a lock on the file used to buffer tracing output.")
    }
}

impl DebugLogger {
    /// Create a new DebugTraceFile instance
    pub(crate) fn new(log_file: Option<&Path>) -> Result<Self, DebuggerError> {
        let mut debug_logger = Self {
            buffer_file: Mutex::new(tempfile()?),
            seek_pointer: SeekFrom::Start(0),
            log_default_guard: None,
        };
        debug_logger.log_default_guard = Some(debug_logger.setup_logging(log_file)?);

        Ok(debug_logger)
    }

    /// Try to clone the buffer file handle with exclusive access, so that it can be used by either
    /// the DebugAdapter to send messages to the DAP client, or the `tracing` module to record log data.
    pub(crate) fn buffer_file_handle(&self) -> Result<File, DebuggerError> {
        self.buffer_file
            .lock()
            .map_err(|error| {
                DebuggerError::Other(anyhow!(
                    "Failed to get a lock on the buffer file. {error:?}"
                ))
            })?
            .try_clone()
            .map_err(|_| {
                DebuggerError::Other(anyhow!(
                    "Failed to get a new file instance to the buffer file."
                ))
            })
    }

    /// Flush the buffer to the DAP client's Debug Console
    pub(crate) fn flush_to_dap(
        &mut self,
        debug_adapter: &mut DebugAdapter<impl ProtocolAdapter>,
    ) -> Result<(), DebuggerError> {
        let read_from_log = self.buffer_file_handle()?;
        let mut tracing_log_handle = BufReader::new(read_from_log.try_clone()?);
        std::io::Seek::seek(&mut tracing_log_handle, self.seek_pointer)?;
        let mut buffer_lines = tracing_log_handle.lines();
        while let Some(Ok(next_line)) = buffer_lines.next() {
            debug_adapter.log_to_console(next_line);
        }
        // Update the seek_pointer to the end of the file, so that we don't re-read the same lines.
        let mut truncate_file = read_from_log.try_clone()?;
        let later_read_pos = std::io::Seek::seek(&mut truncate_file, std::io::SeekFrom::End(0))?;
        self.seek_pointer = std::io::SeekFrom::Start(later_read_pos);
        Ok(())
    }

    /// Flush the buffer to the stderr
    pub(crate) fn flush(&mut self) -> Result<(), DebuggerError> {
        let read_from_log = self.buffer_file_handle()?;
        let mut tracing_log_handle = BufReader::new(read_from_log.try_clone()?);
        std::io::Seek::seek(&mut tracing_log_handle, self.seek_pointer)?;
        let mut buffer_lines = tracing_log_handle.lines();
        while let Some(Ok(next_line)) = buffer_lines.next() {
            eprintln!("{}", next_line);
        }
        // Update the seek_pointer to the end of the file, so that we don't re-read the same lines.
        let mut truncate_file = read_from_log.try_clone()?;
        let later_read_pos = std::io::Seek::seek(&mut truncate_file, std::io::SeekFrom::End(0))?;
        self.seek_pointer = std::io::SeekFrom::Start(later_read_pos);
        Ok(())
    }

    /// Setup logging, according to the following rules.
    /// 1. If the RUST_LOG environment variable is set, use it as a `LevelFilter` to configure a subscriber that
    ///     logs to the given destination, or default to `RUST_LOG=probe_rs_debug=warn`
    /// 2. If no `log_file` destination is supplied, output will be written to the DAP client's Debug Console,
    /// 3. Irrespective of the RUST_LOG environment variable, configure a subscriber that will write with `LevelFilter::ERROR` to stderr,
    ///     because these errors are picked up and reported to the user by the VSCode extension, when no DAP session is available.
    pub(crate) fn setup_logging(
        &mut self,
        log_file: Option<&Path>,
    ) -> Result<DefaultGuard, DebuggerError> {
        let environment_filter = if std::env::var("RUST_LOG").is_ok() {
            EnvFilter::from_default_env()
        } else {
            EnvFilter::new("probe_rs=warn")
        };
        Ok(match log_file {
            Some(log_path) => {
                let log_file = File::create(log_path)?;
                let log_message = format!(
                    "Log output for {:?} will be written to: {:?}",
                    &environment_filter.to_string(),
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
                log_default_guard
            }
            None => {
                if let Some(max_level) = environment_filter.max_level_hint() {
                    if max_level == LevelFilter::TRACE {
                        return Err(DebuggerError::UserMessage(
                                    format!("{}{}",
                                    "Using the `TRACE` log level to stream data to the console may have adverse effects on performance. ",
                                    "Consider using a less verbose log level, or use one of the `logFile` or `logToDir` options."
                                )));
                    }
                }

                let log_message = format!(
                    "Log output for {:?} will be written to the Debug Console.",
                    &environment_filter.to_string()
                );

                // If no log file desitination is specified, send logs via the buffer file, to the DAP
                // client's Debug Console.
                let buffer_layer = tracing_subscriber::fmt::layer()
                    .compact()
                    .with_ansi(false)
                    .without_time()
                    .with_line_number(true)
                    .with_span_events(FmtSpan::FULL)
                    .with_writer(BoxMakeWriter::new(self.make_writer()))
                    .with_filter(environment_filter);

                let log_default_guard = tracing_subscriber::registry()
                    .with(buffer_layer)
                    .set_default();
                // Tell the user where RUST_LOG messages are written.
                self.log_to_console(&log_message)?;
                log_default_guard
            }
        })
    }

    /// We can send messages directly to the console, irrespective of log levels, by writing to the `buffer_file`.
    /// If no `buffer_file` is available, we write to `stderr`.
    pub(crate) fn log_to_console(&mut self, message: &str) -> Result<(), DebuggerError> {
        let read_from_log = self.buffer_file_handle()?;
        let mut tracing_log_append_handle = read_from_log.try_clone()?;
        std::io::Seek::seek(&mut tracing_log_append_handle, std::io::SeekFrom::End(0))?;
        let mut tracing_log_handle = LineWriter::new(tracing_log_append_handle);
        tracing_log_handle.write_all(format!("probe-rs-debug: {}\n", message).as_bytes())?;
        tracing_log_handle.flush()?;
        // Make sure we reset the `seek_pointer` to what it was before we wrote to the file,
        // so that we can send this data to the console.
        let mut truncate_file = read_from_log.try_clone()?;
        let _ = std::io::Seek::seek(&mut truncate_file, self.seek_pointer)?;
        Ok(())
    }
}
