use std::time::Instant;

use probe_rs::{Core, MemoryInterface, RegisterId, Session};

use crate::{registers::PC, Elf, TargetInfo, TIMEOUT};

const CANARY_U8: u8 = 0xAA;
const CANARY_U32: u32 = u32::from_le_bytes([CANARY_U8, CANARY_U8, CANARY_U8, CANARY_U8]);

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
/// The whole canary is initialized to `CANARY_U8` before the target program is started.
/// The canary size is 10% of the available stack space or 1 KiB, whichever is smallest.
///
/// When the programs ends (due to panic or breakpoint) the integrity of the canary is checked. If it was
/// "touched" (any of its bytes != `CANARY_U8`) then that is considered to be a *potential* stack
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
            Some(stack_info) => stack_info,
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
        let stack_available = *stack_info.range.end() - stack_start;

        let size = if measure_stack {
            // When measuring stack consumption, we have to color the whole stack.
            stack_available as usize
        } else {
            // We consider >90% stack usage a potential stack overflow, but don't go beyond 1 kb
            // since filling a lot of RAM is slow (and 1 kb should be "good enough" for what we're
            // doing).
            round_up(1024.min(stack_available / 10), 4) as usize
        };

        log::debug!(
            "{stack_available} bytes of stack available ({:#010X} ..= {:#010X}), using {size} byte canary",
            stack_info.range.start(),
            stack_info.range.end(),
        );

        let size_kb = size as f64 / 1024.0;
        if measure_stack {
            // Painting 100KB or more takes a few seconds, so provide user feedback.
            log::info!("painting {size_kb:.2} KiB of RAM for stack usage estimation");
        }
        let start = Instant::now();
        paint_subroutine::execute(&mut core, stack_start, size as u32)?;
        let seconds = start.elapsed().as_secs_f64();
        log::trace!(
            "setting up canary took {seconds:.3}s ({:.2} KiB/s)",
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
            log::info!("reading {size_kb:.2} KiB of RAM for stack usage estimation");
        }
        let start = Instant::now();
        let touched_address = measure_subroutine::execute(core, self.address, self.size as u32)?;
        let seconds = start.elapsed().as_secs_f64();
        log::trace!(
            "reading canary took {seconds:.3}s ({:.2} KiB/s)",
            size_kb / seconds
        );

        let min_stack_usage = match touched_address {
            Some(touched_address) => {
                log::debug!("canary was touched at {touched_address:#010X}");
                Some(elf.vector_table.initial_stack_pointer - touched_address as u32)
            }
            None => None,
        };

        if self.measure_stack {
            let min_stack_usage = min_stack_usage.unwrap_or(0);
            let used_kb = min_stack_usage as f64 / 1024.0;
            let avail_kb = self.stack_available as f64 / 1024.0;
            let pct = used_kb / avail_kb * 100.0;
            log::info!(
                "program has used at least {used_kb:.2}/{avail_kb:.2} KiB ({pct:.1}%) of stack space"
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
                        "program has used at least {used_kb:.2}/{avail_kb:.2} KiB ({pct:.1}%) of stack space",
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

/// Rounds up to the next multiple of `k` that is greater or equal to `n`.
fn round_up(n: u32, k: u32) -> u32 {
    let rem = n % k;
    if rem == 0 {
        n
    } else {
        n + k - rem
    }
}

/// Assert 4-byte-alignment and that subroutine fits inside stack.
macro_rules! assert_subroutine {
    ($low_addr:expr, $stack_size:expr, $subroutine_size:expr) => {
        assert_eq!($low_addr % 4, 0, "low_addr needs to be 4-byte-aligned");
        assert_eq!($stack_size % 4, 0, "stack_size needs to be 4-byte-aligned");
        assert_eq!(
            $subroutine_size % 4,
            0,
            "subroutine needs to be 4-byte-aligned"
        );
        assert!(
            $subroutine_size < $stack_size,
            "subroutine does not fit inside stack"
        );
    };
}

/// Write [`CANARY_U32`] to the stack.
///
/// ### Corresponds to following rust code
///
/// ```rust
/// unsafe fn paint(low_addr: u32, high_addr: u32, pattern: u32) {
///     while low_addr <= high_addr {
///         (low_addr as *mut u32).write(pattern);
///         low_addr += 4;
///     }
/// }
/// ```  
///
/// ### Generated assembly
///
/// The assembly is generated from aboves rust code, using the jorge-hack.
///
/// ```armasm
/// 000200ec <paint>:
///    200ec:    4288    cmp      r0, r1
///    200ee:    d801    bhi.n    200f4 <paint+0x8>
///    200f0:    c004    stmia    r0!, {r2}
///    200f2:    e7fb    b.n      200ec <paint>
///
/// 000200f4 <paint+0x8>:
///    200f4:    be00    bkpt     0x0000
/// ```
///
/// ### Register-parameter-mapping
///
/// - r0: low_addr
/// - r1: high_addr
/// - r2: pattern
mod paint_subroutine {
    use super::*;

    /// Execute the subroutine.
    ///
    /// ## Assumptions
    /// - Expects the [`Core`] to be halted and will leave it halted when the function
    /// returns.
    /// - `low_addr` and `size` need to be 4-byte-aligned.
    pub fn execute(core: &mut Core, low_addr: u32, stack_size: u32) -> Result<(), probe_rs::Error> {
        assert_subroutine!(low_addr, stack_size, self::SUBROUTINE.len() as u32);

        // prepare subroutine
        let previous_pc = super::prepare_subroutine(core, low_addr, stack_size, self::SUBROUTINE)?;

        // execute subroutine and wait for it to finish
        core.run()?;
        core.wait_for_core_halted(TIMEOUT)?;

        // overwrite subroutine
        core.write_8(low_addr as u64, &[CANARY_U8; self::SUBROUTINE.len()])?;

        // reset PC to where it was before
        core.write_core_reg(PC, previous_pc)
    }

    const SUBROUTINE: [u8; 12] = [
        0x88, 0x42, // cmp      r0, r1
        0x01, 0xd8, // bhi.n    200f4 <paint+0x8>
        0x04, 0xc0, // stmia    r0!, {r2}
        0xfb, 0xe7, // b.n      200ec <paint>
        0x00, 0xbe, // bkpt     0x0000
        0x00, 0xbe, // bkpt     0x0000 (padding instruction)
    ];
}

/// Search for lowest touched address in memory.
///
/// ### Corresponds to following rust code
///
/// ```rust
/// #[export_name = "measure"]
/// unsafe fn measure(mut low_addr: u32, high_addr: u32, pattern: u32) -> u32 {
///     let mut result = 0;
///
///     while low_addr < high_addr {
///         if (low_addr as *const u32).read() != pattern {
///             result = low_addr;
///             break;
///         } else {
///             low_addr += 4;
///         }
///     }
///
///     result
/// }
/// ```
///
/// ### Generated assembly
///
/// The assembly is generated from aboves rust code, using the jorge-hack.
///
/// ```armasm
/// 000200ec <measure>:
///     200ec:    4288    cmp      r0, r1
///     200ee:    d204    bcs.n    200fa <measure+0xe>
///     200f0:    6803    ldr      r3, [r0, #0]
///     200f2:    4293    cmp      r3, r2
///     200f4:    d102    bne.n    200fc <measure+0x10>
///     200f6:    1d00    adds     r0, r0, #4
///     200f8:    e7f8    b.n      200ec <measure>
///
/// 000200fa <measure+0xe>:
///     200fa:    2000    movs     r0, #0
///
/// 000200fc <measure+0x10>:
///     200fc:    be00    bkpt     0x0000
/// //                    ^^^^ this was `bx lr`
/// ```
///
/// ### Register-parameter-mapping
///
/// - r0: low_addr, and return value
/// - r1: high_addr
/// - r2: pattern
mod measure_subroutine {
    use super::*;

    /// Execute the subroutine.
    ///
    /// The returned `Option<u32>` is None, if the memory is untouched. Otherwise it
    /// gives the position of the first (lowest) 4-byte-word which isn't the same as
    /// the pattern anymore. Because we are searching for 4-byte-words, the returned
    /// `u32` will always be a multiple of 4.
    ///
    /// ## Assumptions
    /// - Expects the [`Core`] to be halted and will leave it halted when the function
    /// returns.
    /// - `low_addr` and `size` need to be 4-byte-aligned.
    pub fn execute(
        core: &mut Core,
        low_addr: u32,
        stack_size: u32,
    ) -> Result<Option<u32>, probe_rs::Error> {
        assert_subroutine!(low_addr, stack_size, self::SUBROUTINE.len() as u32);

        // use probe to search through the memory the subroutine will be written to
        let mut buf = [0; self::SUBROUTINE.len()];
        core.read_8(low_addr as u64, &mut buf)?;
        match buf.iter().position(|b| *b != CANARY_U8) {
            // if we find a touched value, we return early
            Some(pos) => return Ok(Some(pos as u32)),
            // otherwise continue
            None => {}
        }

        // prepare subroutine
        let _ = super::prepare_subroutine(core, low_addr, stack_size, self::SUBROUTINE)?;

        // execute the subroutine and wait for it to finish
        core.run()?;
        core.wait_for_core_halted(TIMEOUT)?;

        // read out and return the result
        match core.read_core_reg(RegisterId(0))? {
            0 => Ok(None),
            n => Ok(Some(n)),
        }
    }

    const SUBROUTINE: [u8; 20] = [
        0x88, 0x42, // cmp      r0, r1
        0x04, 0xd2, // bcs.n    200fa <measure+0xe>
        0x03, 0x68, // ldr      r3, [r0, #0]
        0x93, 0x42, // cmp      r3, r2
        0x02, 0xd1, // bne.n    200fc <measure+0x10>
        0x00, 0x1d, // adds     r0, r0, #4
        0xf8, 0xe7, // b.n      200ec <measure>
        0x00, 0x20, // movs     r0, #0
        0x00, 0xbe, // bkpt     0x0000
        0x00, 0xbe, // bkpt     0x0000 (padding instruction)
    ];
}

/// Prepare target to execute subroutine.
///
/// After calling this function, the program counter will be at the beginning of
/// the subroutine.
///
/// `low_addr` and `high_addr` need to be 4-byte-aligned.
fn prepare_subroutine<const N: usize>(
    core: &mut Core,
    low_addr: u32,
    stack_size: u32,
    subroutine: [u8; N],
) -> Result<u32, probe_rs::Error> {
    let subroutine_size = N as u32;

    // calculate highest address of stack
    let high_addr = low_addr + stack_size;

    // set the registers
    // NOTE: add `subroutine_size` to `low_addr`, to avoid the subroutine overwriting itself
    core.write_core_reg(RegisterId(0), low_addr + subroutine_size)?;
    core.write_core_reg(RegisterId(1), high_addr)?;
    core.write_core_reg(RegisterId(2), CANARY_U32)?;

    // write subroutine to stack
    core.write_8(low_addr as u64, &subroutine)?;

    // store current PC and set PC to beginning of subroutine
    let previous_pc = core.read_core_reg(PC)?;
    core.write_core_reg(PC, low_addr)?;

    Ok(previous_pc)
}
