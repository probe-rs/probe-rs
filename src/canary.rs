use std::time::Instant;

use probe_rs::{Core, MemoryInterface, Session};

use crate::{registers::PC, Elf, TargetInfo, TIMEOUT};

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
    /// Decide if and where to place the stack canary.
    pub(crate) fn install(
        sess: &mut Session,
        target_info: &TargetInfo,
        elf: &Elf,
        measure_stack: bool,
    ) -> Result<Option<Self>, anyhow::Error> {
        let mut core = sess.core(0)?;
        core.reset_and_halt(TIMEOUT)?;

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

        let stack_start = *stack_info.range.start();
        let stack_end = *stack_info.range.end() - 1;
        let stack_available = stack_end - stack_start;

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
        let start = Instant::now();
        paint_stack(&mut core, stack_start, stack_start + size as u32)?;
        let seconds = start.elapsed().as_secs_f64();
        log::trace!(
            "setting up canary took {:.3}s ({:.2} KiB/s)",
            seconds,
            size_kb / seconds
        );

        Ok(Some(Canary {
            address: stack_start,
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

/// Write [`CANARY_VALUE`] to the stack.
fn paint_stack(core: &mut Core, start: u32, mut end: u32) -> Result<(), probe_rs::Error> {
    // make sure stack_end is properly aligned
    if end % 4 != 0 {
        end += end % 4;
    }

    // construct subroutine
    let subroutine = subroutine(start + SUBROUTINE_LENGTH, end);

    // does the subroutine fit inside the stack?
    let stack_size = (end - start) as usize;
    assert!(
        subroutine.len() < stack_size,
        "subroutine doesn't fit inside stack"
    );

    // write subroutine to RAM
    core.write_8(start, &subroutine)?;

    // store current PC and set PC to beginning of subroutine
    let previous_pc = core.read_core_reg(PC)?;
    core.write_core_reg(PC, start)?;

    // execute the subroutine and wait for it to finish
    core.run()?;
    core.wait_for_core_halted(TIMEOUT).unwrap();

    // overwrite subroutine
    core.write_8(start, &vec![CANARY_VALUE; subroutine.len()])?;

    // reset PC to where it was before
    core.write_core_reg(PC, previous_pc)?;

    Ok(())
}

/// The length of the subroutine.
///
/// This is checked on runtime and needs to be updated when changing the subroutine.
const SUBROUTINE_LENGTH: u32 = 28;

/// Create a subroutine to paint [`CANARY_VALUE`] from `start` till `end`.
///
/// Both `start` and `end` need to be 4-byte-aligned.
//
// Roughly corresponds to following assembly:
//
// 00000108 <start>:
//  108:   4803        ldr r0, [pc, #12]   ; (118 <end+0x2>)
//  10a:   4904        ldr r1, [pc, #16]   ; (11c <end+0x6>)
//  10c:   4a04        ldr r2, [pc, #16]   ; (120 <end+0xa>)
//
// 0000010e <loop>:
//  10e:   4281        cmp r1, r0
//  110:   d001        beq.n   116 <end>
//  112:   c004        stmia   r0!, {r2}
//  114:   e7fb        b.n 10e <loop>
//
// 00000116 <end>:
//  116:   be00        bkpt    0x0000
//  118:   20000100    .word   0x20000100  ; start
//  11c:   20000200    .word   0x20000200  ; end
//  120:   aaaaaaaa    .word   0xaaaaaaaa  ; pattern
fn subroutine(start: u32, end: u32) -> Vec<u8> {
    assert!(start < end, "start needs to be smaller than end address");
    assert_eq!(start % 4, 0, "`start` needs to be 4-byte-aligned");
    assert_eq!(end % 4, 0, "`end` needs to be 4-byte-aligned");

    // convert start and end address to bytes
    let start_bytes = start.to_le_bytes();
    let end_bytes = end.to_le_bytes();

    let mut subroutine = vec![
        0x03, 0x48, // ldr
        0x04, 0x49, // ldr
        0x04, 0x4a, // ldr
        0x81, 0x42, // cmp
        0x01, 0xD0, // beq.n
        0x04, 0xC0, // stmia
        0xFB, 0xE7, // b.n
        0x00, 0xBE, // bkpt
    ];

    // add the start and end address
    subroutine.extend(start_bytes);
    subroutine.extend(end_bytes);

    // add the pattern to be painted
    subroutine.extend([CANARY_VALUE; 4]);

    assert_eq!(subroutine.len() as u32, SUBROUTINE_LENGTH);

    subroutine
}
