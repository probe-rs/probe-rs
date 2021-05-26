use std::path::Path;

use probe_rs::{config::RamRegion, Core};

use crate::elf::ProcessedElf;

mod pp;
mod symbolicate;
mod unwind;

pub(crate) struct Settings<'p> {
    pub(crate) current_dir: &'p Path,
    pub(crate) max_backtrace_len: u32,
    pub(crate) force_backtrace: bool,
    pub(crate) shorten_paths: bool,
}

/// (virtually) unwinds the target's program and prints its backtrace
pub(crate) fn print(
    core: &mut Core,
    elf: &ProcessedElf,
    sp_ram_region: &Option<RamRegion>,
    settings: &Settings,
) -> anyhow::Result<Outcome> {
    let unwind = unwind::target(core, elf, sp_ram_region);

    let frames = symbolicate::frames(&unwind.raw_frames, settings.current_dir, elf);

    let contains_exception = unwind
        .raw_frames
        .iter()
        .any(|raw_frame| raw_frame.is_exception());

    let print_backtrace = settings.force_backtrace
        || unwind.outcome == Outcome::StackOverflow
        || unwind.corrupted
        || contains_exception;

    if print_backtrace && settings.max_backtrace_len > 0 {
        pp::backtrace(&frames, &settings);

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
