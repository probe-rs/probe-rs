//! unwind target's program

use anyhow::{anyhow, Context as _};
use gimli::{BaseAddresses, DebugFrame, UninitializedUnwindContext, UnwindSection as _};
use probe_rs::{config::RamRegion, Core};

use crate::{
    backtrace::Outcome,
    cortexm,
    elf::Elf,
    registers::{self, Registers},
    stacked::Stacked,
};

fn missing_debug_info(pc: u32) -> String {
    format!("debug information for address {:#x} is missing. Likely fixes:
        1. compile the Rust code with `debug = 1` or higher. This is configured in the `profile.{{release,bench}}` sections of Cargo.toml (`profile.{{dev,test}}` default to `debug = 2`)
        2. use a recent version of the `cortex-m` crates (e.g. cortex-m 0.6.3 or newer). Check versions in Cargo.lock
        3. if linking to C code, compile the C code with the `-g` flag", pc)
}

/// Virtually* unwinds the target's program
/// \* destructors are not run
///
/// This returns as much info as could be collected, even if the collection is interrupted by an error.
/// If an error occurred during processing, it is stored in `Output::processing_error`.
pub(crate) fn target(core: &mut Core, elf: &Elf, active_ram_region: &Option<RamRegion>) -> Output {
    let mut output = Output {
        corrupted: true,
        outcome: Outcome::Ok,
        raw_frames: vec![],
        processing_error: None,
    };

    /// Returns all info collected until the error occurred and puts the error into `processing_error`
    macro_rules! unwrap_or_return_output {
        ( $e:expr ) => {
            match $e {
                Ok(x) => x,
                Err(err) => {
                    output.processing_error = Some(anyhow!(err));
                    return output;
                }
            }
        };
    }

    let mut pc = unwrap_or_return_output!(core.read_core_reg(registers::PC));
    let sp = unwrap_or_return_output!(core.read_core_reg(registers::SP));
    let lr = unwrap_or_return_output!(core.read_core_reg(registers::LR));
    let base_addresses = BaseAddresses::default();
    let mut unwind_context = UninitializedUnwindContext::new();
    let mut registers = Registers::new(lr, sp, core);

    loop {
        if let Some(outcome) =
            check_hard_fault(pc, &elf.vector_table, &mut output, sp, active_ram_region)
        {
            output.outcome = outcome;
        }

        output.raw_frames.push(RawFrame::Subroutine { pc });

        let uwt_row = unwrap_or_return_output!(elf
            .debug_frame
            .unwind_info_for_address(
                &base_addresses,
                &mut unwind_context,
                pc.into(),
                DebugFrame::cie_from_offset,
            )
            .with_context(|| missing_debug_info(pc)));

        let cfa_changed = unwrap_or_return_output!(registers.update_cfa(uwt_row.cfa()));

        for (reg, rule) in uwt_row.registers() {
            unwrap_or_return_output!(registers.update(reg, rule));
        }

        let lr = unwrap_or_return_output!(registers.get(registers::LR));

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
        output.corrupted = !cfa_changed && !program_counter_changed;

        if output.corrupted {
            break;
        }

        if exception_entry {
            output.raw_frames.push(RawFrame::Exception);

            // Read the `FType` field from the `EXC_RETURN` value.
            let fpu = lr & cortexm::EXC_RETURN_FTYPE_MASK == 0;

            let sp = unwrap_or_return_output!(registers.get(registers::SP));
            let ram_bounds = active_ram_region
                .as_ref()
                .map(|ram_region| ram_region.range.clone())
                .unwrap_or(cortexm::VALID_RAM_ADDRESS);
            let stacked = if let Some(stacked) =
                unwrap_or_return_output!(Stacked::read(registers.core, sp, fpu, ram_bounds))
            {
                stacked
            } else {
                output.corrupted = true;
                break;
            };

            registers.insert(registers::LR, stacked.lr);
            // adjust the stack pointer for stacked registers
            registers.insert(registers::SP, sp + stacked.size());

            pc = stacked.pc;
        } else {
            if cortexm::is_thumb_bit_set(lr) {
                pc = cortexm::clear_thumb_bit(lr);
            } else {
                output.processing_error = Some(anyhow!(
                    "bug? LR ({:#010x}) didn't have the Thumb bit set",
                    lr
                ));
                return output;
            }
        }
    }

    output
}

fn check_hard_fault(
    pc: u32,
    vector_table: &cortexm::VectorTable,
    output: &mut Output,
    sp: u32,
    sp_ram_region: &Option<RamRegion>,
) -> Option<Outcome> {
    if cortexm::is_hard_fault(pc, vector_table) {
        assert!(
            output.raw_frames.is_empty(),
            "when present HardFault handler must be the first frame we unwind but wasn't"
        );

        if overflowed_stack(sp, sp_ram_region) {
            return Some(Outcome::StackOverflow);
        } else {
            return Some(Outcome::HardFault);
        }
    }
    None
}

#[derive(Debug)]
pub struct Output {
    pub(crate) corrupted: bool,
    pub(crate) outcome: Outcome,
    pub(crate) raw_frames: Vec<RawFrame>,
    /// Will be `Some` if an error occured while putting together the output.
    /// `outcome` and `raw_frames` will contain all info collected until the error occurred.
    pub(crate) processing_error: Option<anyhow::Error>,
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

fn overflowed_stack(sp: u32, active_ram_region: &Option<RamRegion>) -> bool {
    if let Some(active_ram_region) = active_ram_region {
        // NOTE stack is full descending; meaning the stack pointer can be
        // `ORIGIN(RAM) + LENGTH(RAM)`
        let range = active_ram_region.range.start..=active_ram_region.range.end;
        !range.contains(&sp)
    } else {
        log::warn!("no RAM region appears to contain the stack; cannot determine if this was a stack overflow");
        false
    }
}
