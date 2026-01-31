use probe_rs::{
    CoreInterface, MemoryInterface,
    architecture::{riscv::Riscv32, xtensa::Xtensa},
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
