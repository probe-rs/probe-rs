use colored::{Color, Colorize};
#[cfg(feature = "defmt")]
use decoder::Frame;
use io::Write;
use log::{Level, Log, Metadata, Record};
use std::{
    io,
    sync::atomic::{AtomicUsize, Ordering},
};

const DEFMT_TARGET_MARKER: &str = "defmt@";

/// Initializes the `probe-run` logger.
pub fn init(verbose: bool) {
    log::set_boxed_logger(Box::new(Logger {
        verbose,
        timing_align: AtomicUsize::new(8),
    }))
    .unwrap();
    log::set_max_level(log::LevelFilter::Trace);
}

/// Logs a defmt frame using the `log` facade.
#[cfg(feature = "defmt")]
pub fn log_defmt(
    frame: &Frame<'_>,
    file: Option<&str>,
    line: Option<u32>,
    module_path: Option<&str>,
) {
    let level = match frame.level() {
        decoder::Level::Trace => Level::Trace,
        decoder::Level::Debug => Level::Debug,
        decoder::Level::Info => Level::Info,
        decoder::Level::Warn => Level::Warn,
        decoder::Level::Error => Level::Error,
    };

    let target = format!("{}{}", DEFMT_TARGET_MARKER, frame.timestamp());
    let display = frame.display_message();

    log::logger().log(
        &Record::builder()
            .args(format_args!("{}", display))
            .level(level)
            .target(&target)
            .module_path(module_path)
            .file(file)
            .line(line)
            .build(),
    );
}

struct Logger {
    /// Whether to log `debug!` and `trace!`-level host messages.
    verbose: bool,

    /// Number of characters used by the timestamp. This may increase over time and is used to align
    /// messages.
    timing_align: AtomicUsize,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        if metadata.target().starts_with(DEFMT_TARGET_MARKER) {
            // defmt is configured at firmware-level, we will print all of it.
            true
        } else {
            // Host logs use `info!` as the default level, but with the `verbose` flag set we log at
            // `trace!` level instead.
            if self.verbose {
                metadata.target().starts_with("probe_run")
            } else {
                metadata.target().starts_with("probe_run") && metadata.level() <= Level::Info
            }
        }
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let level_color = match record.level() {
            Level::Error => Color::Red,
            Level::Warn => Color::Yellow,
            Level::Info => Color::Green,
            Level::Debug => Color::BrightWhite,
            Level::Trace => Color::BrightBlack,
        };

        let target = record.metadata().target();
        let is_defmt = target.starts_with(DEFMT_TARGET_MARKER);

        let timestamp = if is_defmt {
            let timestamp = target[DEFMT_TARGET_MARKER.len()..].parse::<u64>().unwrap();
            let seconds = timestamp / 1000000;
            let micros = timestamp % 1000000;
            format!("{}.{:06}", seconds, micros)
        } else {
            // Mark host logs.
            format!("(HOST)")
        };

        let mod_path = record.module_path().unwrap_or("");

        self.timing_align
            .fetch_max(timestamp.len(), Ordering::Relaxed);

        let (stdout, stderr, mut stdout_lock, mut stderr_lock);
        let sink: &mut dyn Write = if is_defmt {
            // defmt goes to stdout, since it's the primary output produced by this tool.
            stdout = io::stdout();
            stdout_lock = stdout.lock();
            &mut stdout_lock
        } else {
            // Everything else goes to stderr.
            stderr = io::stderr();
            stderr_lock = stderr.lock();
            &mut stderr_lock
        };

        writeln!(
            sink,
            "{timestamp:>0$} {level:5} {module:9} | {args}",
            self.timing_align.load(Ordering::Relaxed),
            timestamp = timestamp,
            level = record.level().to_string().color(level_color),
            module = mod_path,
            args = record.args(),
        )
        .ok();

        if let Some(file) = record.file() {
            // Always include location info for defmt output.
            if is_defmt || self.verbose {
                let mut loc = file.to_string();
                if let Some(line) = record.line() {
                    loc.push_str(&format!(":{}", line));
                }
                writeln!(sink, "└─ {}", loc.dimmed()).ok();
            }
        }
    }

    fn flush(&self) {}
}
