use std::time::Instant;

use crate::TIMEOUT;
use crate::{Elf, TargetInfo};
use probe_rs::{MemoryInterface, Session};

const CANARY_VALUE: u8 = 0xAA;

/// (Location of) the stack canary
///
/// The stack canary is used to detect *potential* stack overflows
///
/// The canary is placed in memory as shown in the diagram below:
///
/// ``` text
/// +--------+ -> initial_stack_pointer / stack_range.end()
/// |        |
/// | stack  | (grows downwards)
/// |        |
/// +--------+
/// |        |
/// |        |
/// +--------+
/// | canary |
/// +--------+ -> stack_range.start()
/// |        |
/// | static | (variables, fixed size)
/// |        |
/// +--------+ -> lowest RAM address
/// ```
///
/// The whole canary is initialized to `CANARY_VALUE` before the target program is started.
/// The canary size is 10% of the available stack space or 1 KiB, whichever is smallest.
///
/// When the programs ends (due to panic or breakpoint) the integrity of the canary is checked. If it was
/// "touched" (any of its bytes != `CANARY_VALUE`) then that is considered to be a *potential* stack
/// overflow.
#[derive(Clone, Copy)]
pub(crate) struct Canary {
    address: u32,
    size: usize,
    stack_available: u32,
    data_below_stack: bool,
    measure_stack: bool,
}

impl Canary {
    pub(crate) fn install(
        sess: &mut Session,
        target_info: &TargetInfo,
        elf: &Elf,
        measure_stack: bool,
    ) -> Result<Option<Self>, anyhow::Error> {
        let mut core = sess.core(0)?;
        core.reset_and_halt(TIMEOUT)?;

        // Decide if and where to place the stack canary.

        let stack_info = match &target_info.stack_info {
            Some(range) => range,
            None => {
                log::debug!("couldn't find valid stack range, not placing stack canary");
                return Ok(None);
            }
        };

        if elf.program_uses_heap() {
            log::debug!("heap in use, not placing stack canary");
            return Ok(None);
        }

        let stack_available = stack_info.range.end() - stack_info.range.start() - 1;

        let size = if measure_stack {
            // When measuring stack consumption, we have to color the whole stack.
            stack_available as usize
        } else {
            // We consider >90% stack usage a potential stack overflow, but don't go beyond 1 kb
            // since filling a lot of RAM is slow (and 1 kb should be "good enough" for what we're
            // doing).
            1024.min(stack_available / 10) as usize
        };

        log::debug!(
            "{} bytes of stack available ({:#010X} ..= {:#010X}), using {} byte canary",
            stack_available,
            stack_info.range.start(),
            stack_info.range.end(),
            size,
        );

        let size_kb = size as f64 / 1024.0;
        if measure_stack {
            // Painting 100KB or more takes a few seconds, so provide user feedback.
            log::info!(
                "painting {:.2} KiB of RAM for stack usage estimation",
                size_kb
            );
        }
        let address = *stack_info.range.start();
        let canary = vec![CANARY_VALUE; size];
        let start = Instant::now();
        core.write_8(address, &canary)?;
        let seconds = start.elapsed().as_secs_f64();
        log::trace!(
            "setting up canary took {:.3}s ({:.2} KiB/s)",
            seconds,
            size_kb / seconds
        );

        Ok(Some(Canary {
            address,
            size,
            stack_available,
            data_below_stack: stack_info.data_below_stack,
            measure_stack,
        }))
    }

    pub(crate) fn touched(self, core: &mut probe_rs::Core, elf: &Elf) -> anyhow::Result<bool> {
        let size_kb = self.size as f64 / 1024.0;
        if self.measure_stack {
            log::info!(
                "reading {:.2} KiB of RAM for stack usage estimation",
                size_kb,
            );
        }
        let mut canary = vec![0; self.size];
        let start = Instant::now();
        core.read_8(self.address, &mut canary)?;
        let seconds = start.elapsed().as_secs_f64();
        log::trace!(
            "reading canary took {:.3}s ({:.2} KiB/s)",
            seconds,
            size_kb / seconds
        );

        let min_stack_usage = match canary.iter().position(|b| *b != CANARY_VALUE) {
            Some(pos) => {
                let touched_address = self.address + pos as u32;
                log::debug!("canary was touched at {:#010X}", touched_address);

                Some(elf.vector_table.initial_stack_pointer - touched_address)
            }
            None => None,
        };

        if self.measure_stack {
            let min_stack_usage = min_stack_usage.unwrap_or(0);
            let used_kb = min_stack_usage as f64 / 1024.0;
            let avail_kb = self.stack_available as f64 / 1024.0;
            let pct = used_kb / avail_kb * 100.0;
            log::info!(
                "program has used at least {:.2}/{:.2} KiB ({:.1}%) of stack space",
                used_kb,
                avail_kb,
                pct,
            );

            // Don't test for stack overflows if we're measuring stack usage.
            Ok(false)
        } else {
            match min_stack_usage {
                Some(min_stack_usage) => {
                    let used_kb = min_stack_usage as f64 / 1024.0;
                    let avail_kb = self.stack_available as f64 / 1024.0;
                    let pct = used_kb / avail_kb * 100.0;
                    log::warn!(
                        "program has used at least {:.2}/{:.2} KiB ({:.1}%) of stack space",
                        used_kb,
                        avail_kb,
                        pct,
                    );

                    if self.data_below_stack {
                        log::warn!("data segments might be corrupted due to stack overflow");
                    }

                    Ok(true)
                }
                None => {
                    log::debug!("stack canary intact");
                    Ok(false)
                }
            }
        }
    }
}
