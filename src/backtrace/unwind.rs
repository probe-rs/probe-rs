//! unwind target's program

use anyhow::{ensure, Context as _};
use gimli::{
    BaseAddresses, DebugFrame, LittleEndian, UninitializedUnwindContext, UnwindSection as _,
};
use probe_rs::{config::RamRegion, Core};

use crate::{
    cortexm,
    registers::{self, Registers},
    stacked::Stacked,
    Outcome, VectorTable,
};

static MISSING_DEBUG_INFO: &str = "debug information is missing. Likely fixes:
1. compile the Rust code with `debug = 1` or higher. This is configured in the `profile.{release,bench}` sections of Cargo.toml (`profile.{dev,test}` default to `debug = 2`)
2. use a recent version of the `cortex-m` crates (e.g. cortex-m 0.6.3 or newer). Check versions in Cargo.lock
3. if linking to C code, compile the C code with the `-g` flag";

/// Virtually* unwinds the target's program
/// \* destructors are not run
// FIXME(?) this should be "infallible" and return as many frames as possible even in case of IO
// errors
pub(crate) fn target(
    core: &mut Core,
    debug_frame: &[u8],
    vector_table: &VectorTable,
    sp_ram_region: &Option<RamRegion>,
) -> anyhow::Result<Output> {
    let mut debug_frame = DebugFrame::new(debug_frame, LittleEndian);
    debug_frame.set_address_size(cortexm::ADDRESS_SIZE);

    let mut pc = core.read_core_reg(registers::PC)?;
    let sp = core.read_core_reg(registers::SP)?;
    let lr = core.read_core_reg(registers::LR)?;
    let base_addresses = BaseAddresses::default();
    let mut unwind_context = UninitializedUnwindContext::new();

    let mut outcome = Outcome::Ok;
    let mut registers = Registers::new(lr, sp, core);
    let mut raw_frames = vec![];
    let mut corrupted = true;

    loop {
        if cortexm::is_hard_fault(pc, vector_table) {
            assert!(
                raw_frames.is_empty(),
                "when present HardFault handler must be the first frame we unwind but wasn't"
            );

            outcome = if overflowed_stack(sp, sp_ram_region) {
                Outcome::StackOverflow
            } else {
                Outcome::HardFault
            };
        }

        raw_frames.push(RawFrame::Subroutine { pc });

        let uwt_row = debug_frame
            .unwind_info_for_address(
                &base_addresses,
                &mut unwind_context,
                pc.into(),
                DebugFrame::cie_from_offset,
            )
            .with_context(|| MISSING_DEBUG_INFO)?;

        let cfa_changed = registers.update_cfa(uwt_row.cfa())?;

        for (reg, rule) in uwt_row.registers() {
            registers.update(reg, rule)?;
        }

        let lr = registers.get(registers::LR)?;

        log::debug!("LR={:#010X} PC={:#010X}", lr, pc);

        if lr == registers::LR_END {
            break;
        }

        // Link Register contains an EXC_RETURN value. This deliberately also includes
        // invalid combinations of final bits 0-4 to prevent futile backtrace re-generation attempts
        let exception_entry = lr >= cortexm::EXC_RETURN_MARKER;

        let program_counter_changed = !cortexm::subroutine_eq(lr, pc);

        // If the frame didn't move, and the program counter didn't change, bail out (otherwise we
        // might print the same frame over and over).
        corrupted = !cfa_changed && !program_counter_changed;

        if corrupted {
            break;
        }

        if exception_entry {
            raw_frames.push(RawFrame::Exception);

            // Read the `FType` field from the `EXC_RETURN` value.
            let fpu = lr & (1 << 4) == 0;

            let sp = registers.get(registers::SP)?;
            let ram_bounds = sp_ram_region
                .as_ref()
                .map(|ram_region| ram_region.range.clone())
                .unwrap_or(cortexm::VALID_RAM_ADDRESS);
            let stacked = if let Some(stacked) = Stacked::read(registers.core, sp, fpu, ram_bounds)?
            {
                stacked
            } else {
                corrupted = true;
                break;
            };

            registers.insert(registers::LR, stacked.lr);
            // adjust the stack pointer for stacked registers
            registers.insert(registers::SP, sp + stacked.size());

            pc = stacked.pc;
        } else {
            ensure!(
                cortexm::is_thumb_bit_set(lr),
                "bug? LR ({:#010x}) didn't have the Thumb bit set",
                lr
            );

            pc = cortexm::clear_thumb_bit(lr);
        }
    }

    Ok(Output {
        corrupted,
        outcome,
        raw_frames,
    })
}

#[derive(Debug)]
pub struct Output {
    pub(crate) corrupted: bool,
    pub(crate) outcome: Outcome,
    pub(crate) raw_frames: Vec<RawFrame>,
}

/// Backtrace frame prior to 'symbolication'
#[derive(Debug)]
pub(crate) enum RawFrame {
    Subroutine { pc: u32 },
    Exception,
}

impl RawFrame {
    /// Returns `true` if the raw_frame is [`Exception`].
    pub(crate) fn is_exception(&self) -> bool {
        matches!(self, Self::Exception)
    }
}

fn overflowed_stack(sp: u32, sp_ram_region: &Option<RamRegion>) -> bool {
    if let Some(sp_ram_region) = sp_ram_region {
        // NOTE stack is full descending; meaning the stack pointer can be
        // `ORIGIN(RAM) + LENGTH(RAM)`
        let range = sp_ram_region.range.start..=sp_ram_region.range.end;
        !range.contains(&sp)
    } else {
        log::warn!("no RAM region appears to contain the stack; cannot determine if this was a stack overflow");
        false
    }
}
