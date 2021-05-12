use std::{collections::HashSet, path::Path};

use object::read::File as ElfFile;
use probe_rs::{config::RamRegion, Core};

use crate::{Outcome, VectorTable};

mod pp;
mod symbolicate;
mod unwind;

/// (virtually) unwinds the target's program and prints its backtrace
#[allow(clippy::too_many_arguments)]
pub(crate) fn print(
    core: &mut Core,
    debug_frame: &[u8],
    elf: &ElfFile,
    vector_table: &VectorTable,
    sp_ram_region: &Option<RamRegion>,
    live_functions: &HashSet<&str>,
    current_dir: &Path,
    force_backtrace: bool,
    max_backtrace_len: u32,
) -> anyhow::Result<Outcome> {
    let unwind = unwind::target(core, debug_frame, vector_table, sp_ram_region)?;

    let frames = symbolicate::frames(&unwind.raw_frames, live_functions, current_dir, elf);

    let contains_exception = unwind
        .raw_frames
        .iter()
        .any(|raw_frame| raw_frame.is_exception());

    let print_backtrace = force_backtrace
        || unwind.outcome == Outcome::StackOverflow
        || unwind.corrupted
        || contains_exception;

    if print_backtrace && max_backtrace_len > 0 {
        pp::backtrace(&frames, max_backtrace_len);

        if unwind.corrupted {
            log::warn!("call stack was corrupted; unwinding could not be completed");
        }
    }

    Ok(unwind.outcome)
}
