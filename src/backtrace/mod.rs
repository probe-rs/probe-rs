use std::path::Path;

use probe_rs::{config::RamRegion, Core};

use crate::elf::Elf;

mod pp;
mod symbolicate;
mod unwind;

// change as follows:
// --force-backtrace is removed
// --backtrace-len is renamed to --backtrace-limit
// --backtrace is added

// Additionally,
// --backtrace flag is optional and defaults to auto
// --backtrace-limit flag is optional and defaults to 50 (+)
// --backtrace-limit=0 is accepted and means "no limit"
pub(crate) struct Settings<'p> {
    pub(crate) current_dir: &'p Path,
    pub(crate) max_backtrace_len: u32,
    pub(crate) shorten_paths: bool,
}

/// (virtually) unwinds the target's program and prints its backtrace
pub(crate) fn print(
    core: &mut Core,
    elf: &Elf,
    active_ram_region: &Option<RamRegion>,
    settings: &Settings,
) -> anyhow::Result<Outcome> {
    let unwind = unwind::target(core, elf, active_ram_region);

    let frames = symbolicate::frames(&unwind.raw_frames, settings.current_dir, elf);

    let contains_exception = unwind
        .raw_frames
        .iter()
        .any(|raw_frame| raw_frame.is_exception());

        let print_backtrace =
        unwind.outcome == Outcome::StackOverflow
        || unwind.corrupted
        || contains_exception;

    if print_backtrace && settings.max_backtrace_len > 0 {
        pp::backtrace(&frames, settings);

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
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Outcome {
    HardFault,
    Ok,
    StackOverflow,
}

impl Outcome {
    pub(crate) fn log(&self) {
        match self {
            Outcome::StackOverflow => {
                log::error!("the program has overflowed its stack");
            }
            Outcome::HardFault => {
                log::error!("the program panicked");
            }
            Outcome::Ok => {
                log::info!("device halted without error");
            }
        }
    }
}

/// Converts `Outcome` to an exit code.
impl From<Outcome> for i32 {
    fn from(outcome: Outcome) -> i32 {
        match outcome {
            Outcome::HardFault | Outcome::StackOverflow => crate::SIGABRT,
            Outcome::Ok => 0,
        }
    }
}
