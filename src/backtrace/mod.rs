use std::path::Path;

use probe_rs::{config::RamRegion, Core};
use signal_hook::consts::signal;

use crate::elf::Elf;

mod pp;
mod symbolicate;
mod unwind;

#[derive(PartialEq, Eq)]
pub enum BacktraceOptions {
    Auto,
    Never,
    Always,
}

impl From<&String> for BacktraceOptions {
    fn from(item: &String) -> Self {
        match item.as_str() {
            "auto" | "Auto" => BacktraceOptions::Auto,
            "never" | "Never" => BacktraceOptions::Never,
            "always" | "Always" => BacktraceOptions::Always,
            _ => panic!("options for `--backtrace` are `auto`, `never`, `always`."),
        }
    }
}

pub struct Settings<'p> {
    pub current_dir: &'p Path,
    pub backtrace: BacktraceOptions,
    pub panic_present: bool,
    pub backtrace_limit: u32,
    pub shorten_paths: bool,
    pub include_addresses: bool,
}

/// (virtually) unwinds the target's program and prints its backtrace
pub fn print(
    core: &mut Core,
    elf: &Elf,
    active_ram_region: &Option<RamRegion>,
    settings: &mut Settings<'_>,
) -> anyhow::Result<Outcome> {
    let unwind = unwind::target(core, elf, active_ram_region);

    let frames = symbolicate::frames(&unwind.raw_frames, settings.current_dir, elf);

    let contains_exception = unwind
        .raw_frames
        .iter()
        .any(|raw_frame| raw_frame.is_exception());

    let print_backtrace = match settings.backtrace {
        BacktraceOptions::Never => false,
        BacktraceOptions::Always => true,
        BacktraceOptions::Auto => {
            settings.panic_present
                || unwind.outcome == Outcome::StackOverflow
                || unwind.corrupted
                || contains_exception
        }
    };

    // `0` disables the limit and we want to show _all_ frames
    if settings.backtrace_limit == 0 {
        settings.backtrace_limit = frames.len() as u32;
    }

    if print_backtrace && settings.backtrace_limit > 0 {
        pp::backtrace(&frames, settings)?;

        if unwind.corrupted {
            log::warn!("call stack was corrupted; unwinding could not be completed");
        }
        if let Some(err) = unwind.processing_error {
            log::error!(
                "error occurred during backtrace creation: {:?}\n               \
                         the backtrace may be incomplete.",
                err
            );
        }
    }

    Ok(unwind.outcome)
}

/// Target program outcome
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    HardFault,
    Ok,
    StackOverflow,
    /// Control-C was pressed
    CtrlC,
}

impl Outcome {
    pub fn log(&self) {
        match self {
            Outcome::StackOverflow => log::error!("the program has overflowed its stack"),
            Outcome::HardFault => log::error!("the program panicked"),
            Outcome::Ok => log::info!("device halted without error"),
            Outcome::CtrlC => log::info!("device halted by user"),
        }
    }
}

// Convert `Outcome` to an exit code.
impl From<Outcome> for i32 {
    fn from(outcome: Outcome) -> i32 {
        match outcome {
            Outcome::HardFault | Outcome::StackOverflow => signal::SIGABRT,
            Outcome::CtrlC => signal::SIGINT,
            Outcome::Ok => 0,
        }
    }
}
