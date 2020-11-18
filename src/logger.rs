use ansi_term::Colour;
use colored::{Color, Colorize};
use defmt_decoder::Frame;
use difference::{Changeset, Difference};
use log::{Level, Log, Metadata, Record};

use std::{
    fmt::Write as _,
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
pub fn log_defmt(
    frame: &Frame<'_>,
    file: Option<&str>,
    line: Option<u32>,
    module_path: Option<&str>,
) {
    let level = match frame.level() {
        defmt_decoder::Level::Trace => Level::Trace,
        defmt_decoder::Level::Debug => Level::Debug,
        defmt_decoder::Level::Info => Level::Info,
        defmt_decoder::Level::Warn => Level::Warn,
        defmt_decoder::Level::Error => Level::Error,
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

        self.timing_align
            .fetch_max(timestamp.len(), Ordering::Relaxed);

        let (stdout, stderr, mut stdout_lock, mut stderr_lock);
        let sink: &mut dyn io::Write = if is_defmt {
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
            "{timestamp:>0$} {level:5} {args}",
            self.timing_align.load(Ordering::Relaxed),
            timestamp = timestamp,
            level = record.level().to_string().color(level_color),
            args = color_diff(record.args().to_string()),
        )
        .ok();

        if let Some(file) = record.file() {
            // NOTE will be `Some` if `file` is `Some`
            let mod_path = record.module_path().unwrap();
            // Always include location info for defmt output.
            if is_defmt || self.verbose {
                let mut loc = file.to_string();
                if let Some(line) = record.line() {
                    loc.push_str(&format!(":{}", line));
                }
                writeln!(sink, "{}", format!("└─ {} @ {}", mod_path, loc).dimmed()).ok();
            }
        }
    }

    fn flush(&self) {}
}

// color the output of `defmt::assert_eq`
// HACK we should not re-parse formatted output but instead directly format into a color diff
// template; that may require specially tagging log messages that come from `defmt::assert_eq`
fn color_diff(text: String) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    let nlines = lines.len();
    if nlines > 2 {
        let left = lines[nlines - 2];
        let right = lines[nlines - 1];

        const LEFT_START: &str = " left: `";
        const RIGHT_START: &str = "right: `";
        const END: &str = "`";
        if left.starts_with(LEFT_START)
            && left.ends_with(END)
            && right.starts_with(RIGHT_START)
            && right.ends_with(END)
        {
            let left = &left[LEFT_START.len()..left.len() - END.len()];
            let right = &right[RIGHT_START.len()..right.len() - END.len()];

            let mut buf = lines[..nlines - 2].join("\n").bold().to_string();
            buf.push('\n');

            let changeset = Changeset::new(left, right, "");

            writeln!(
                buf,
                "{} {} / {}",
                "diff".bold(),
                "< left".red(),
                "right >".green()
            )
            .ok();
            write!(buf, "{}", "<".red()).ok();
            for diff in &changeset.diffs {
                match diff {
                    Difference::Same(s) => {
                        write!(buf, "{}", s.red()).ok();
                    }
                    Difference::Add(_) => continue,
                    Difference::Rem(s) => {
                        write!(buf, "{}", Colour::Red.on(Colour::Fixed(52)).bold().paint(s)).ok();
                    }
                }
            }
            buf.push('\n');

            write!(buf, "{}", ">".green()).ok();
            for diff in &changeset.diffs {
                match diff {
                    Difference::Same(s) => {
                        write!(buf, "{}", s.green()).ok();
                    }
                    Difference::Rem(_) => continue,
                    Difference::Add(s) => {
                        write!(
                            buf,
                            "{}",
                            Colour::Green.on(Colour::Fixed(22)).bold().paint(s)
                        )
                        .ok();
                    }
                }
            }
            return buf;
        }
    }

    text.bold().to_string()
}
