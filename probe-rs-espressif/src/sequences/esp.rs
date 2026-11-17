use std::ops::Range;

use probe_rs::{
    CoreInterface, MemoryInterface, Session,
    architecture::{riscv::Riscv32, xtensa::Xtensa},
    config::MemoryRegion,
    flashing::{FlashError, FlashProgress, Flasher},
    semihosting::{SemihostingCommand, UnknownCommandDetails, WriteConsoleRequest},
};

pub(super) struct EspBreakpointHandler {}

impl EspBreakpointHandler {
    pub fn handle_riscv_idf_semihosting(
        arch: &mut Riscv32,
        details: UnknownCommandDetails,
    ) -> Result<Option<SemihostingCommand>, crate::Error> {
        match details.operation {
            0x103 => {
                // ESP_SEMIHOSTING_SYS_BREAKPOINT_SET. Can be either set or clear breakpoint, and
                // depending on the operation the parameter pointer points to 2 or 3 words.
                let set_breakpoint = arch.read_word_32(details.parameter as u64)?;
                if set_breakpoint != 0 {
                    let mut breakpoint_data = [0; 2];
                    arch.read_32(details.parameter as u64 + 4, &mut breakpoint_data)?;
                    let [breakpoint_number, address] = breakpoint_data;
                    arch.set_hw_breakpoint(breakpoint_number as usize, address as u64)?;
                } else {
                    let breakpoint_number = arch.read_word_32(details.parameter as u64 + 4)?;
                    arch.clear_hw_breakpoint(breakpoint_number as usize)?;
                }
                Ok(None)
            }
            0x116 => Self::read_panic_reason(arch, details.parameter),
            _ => Ok(Some(SemihostingCommand::Unknown(details))),
        }
    }
    pub fn handle_xtensa_idf_semihosting(
        arch: &mut Xtensa,
        details: UnknownCommandDetails,
    ) -> Result<Option<SemihostingCommand>, crate::Error> {
        match details.operation {
            // There is no 0x103, Xtensa CPUs can set a breakpoint without semihosting
            0x116 => Self::read_panic_reason(arch, details.parameter),
            _ => Ok(Some(SemihostingCommand::Unknown(details))),
        }
    }

    /// Handles ESP_SEMIHOSTING_SYS_PANIC_REASON by turning it into a `WriteConsoleRequest` command.
    fn read_panic_reason(
        arch: &mut dyn CoreInterface,
        parameter: u32,
    ) -> Result<Option<SemihostingCommand>, crate::Error> {
        let mut buffer = [0; 2];
        arch.read_32(parameter as u64, &mut buffer)?;

        let [address, length] = buffer;

        Ok(Some(SemihostingCommand::WriteConsole(
            WriteConsoleRequest::new(address, Some(length)),
        )))
    }
}

/// Caps the flash region and its aliases at the flash size that the flash algorithm reads
/// from the flash chip.
pub(super) fn detect_flash_size(
    session: &mut Session,
    memory_map: &mut [MemoryRegion],
) -> Result<(), FlashError> {
    let target = session.target();
    let mut flasher = Flasher::new(target, 0, &target.flash_algorithms[0])?;

    let size = flasher.run_verify(session, &mut FlashProgress::empty(), |flasher, _| {
        flasher.call_vendor_function("FlashSize", [None; 4])
    })?;

    if size == 0 {
        return Err(FlashError::RoutineCallFailed {
            name: "FlashSize".to_string(),
            error_code: size,
        });
    }

    for region in memory_map {
        if let MemoryRegion::Nvm(region) = region {
            cap_region(&mut region.range, size.into());
        }
    }

    Ok(())
}

// An alias window maps flash pages through the MMU, so it cannot address more distinct flash
// than the chip has.
fn cap_region(range: &mut Range<u64>, size: u64) {
    range.end = range.end.min(range.start + size);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_region_shrinks_larger_region() {
        let mut range = 0x4200_0000..0x4300_0000;
        cap_region(&mut range, 0x40_0000);
        assert_eq!(range, 0x4200_0000..0x4240_0000);
    }

    #[test]
    fn cap_region_keeps_smaller_region() {
        let mut range = 0x400c_2000..0x40c0_0000;
        cap_region(&mut range, 0x100_0000);
        assert_eq!(range, 0x400c_2000..0x40c0_0000);
    }

    #[test]
    fn cap_region_keeps_region_of_equal_size() {
        let mut range = 0x0..0x100_0000;
        cap_region(&mut range, 0x100_0000);
        assert_eq!(range, 0x0..0x100_0000);
    }
}
