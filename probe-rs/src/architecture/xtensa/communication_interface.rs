//! Xtensa Debug Module Communication

// TODO: remove
#![allow(missing_docs)]

use crate::{
    architecture::xtensa::arch::{instruction, CpuRegister, Register, SpecialRegister},
    probe::JTAGAccess,
    DebugProbeError, MemoryInterface,
};

use super::xdm::{Error as XdmError, Xdm};

/// Possible Xtensa errors
#[derive(thiserror::Error, Debug)]
pub enum XtensaError {
    /// An error originating from the DebugProbe
    #[error("Debug Probe Error")]
    DebugProbe(#[from] DebugProbeError),
    /// Xtensa debug module error
    #[error("Xtensa debug module error: {0}")]
    XdmError(XdmError),
}

struct XtensaCommunicationInterfaceState {
    /// Pairs of (register, value). TODO: can/should we handle special registers?
    saved_registers: Vec<(Register, u32)>,

    print_exception_cause: bool,
}

/// A interface that implements controls for Xtensa cores.
pub struct XtensaCommunicationInterface {
    /// The Xtensa debug module
    xdm: Xdm,

    state: XtensaCommunicationInterfaceState,
}

impl XtensaCommunicationInterface {
    /// Create the Xtensa communication interface using the underlying probe driver
    pub fn new(probe: Box<dyn JTAGAccess>) -> Result<Self, (Box<dyn JTAGAccess>, DebugProbeError)> {
        let xdm = Xdm::new(probe).map_err(|(probe, e)| match e {
            XtensaError::DebugProbe(err) => (probe, err),
            other_error => (probe, DebugProbeError::Other(other_error.into())),
        })?;

        let s = Self {
            xdm,
            state: XtensaCommunicationInterfaceState {
                saved_registers: vec![],
                print_exception_cause: true,
            },
        };

        Ok(s)
    }

    pub fn enter_ocd_mode(&mut self) -> Result<(), XtensaError> {
        self.xdm.enter_ocd_mode()?;
        tracing::info!("Entered OCD mode");
        Ok(())
    }

    pub fn is_in_ocd_mode(&mut self) -> Result<bool, XtensaError> {
        self.xdm.is_in_ocd_mode()
    }

    pub fn leave_ocd_mode(&mut self) -> Result<(), XtensaError> {
        self.resume()?;
        self.xdm.leave_ocd_mode()?;
        tracing::info!("Left OCD mode");
        Ok(())
    }

    pub fn halt(&mut self) -> Result<(), XtensaError> {
        self.xdm.halt()?;
        Ok(())
    }

    pub fn resume(&mut self) -> Result<(), XtensaError> {
        self.restore_registers()?;
        self.xdm.resume()?;

        Ok(())
    }

    fn read_cpu_register(&mut self, register: CpuRegister) -> Result<u32, XtensaError> {
        self.execute_instruction(instruction::rsr(SpecialRegister::Ddr, register))?;
        self.xdm.read_ddr()
    }

    fn read_special_register(&mut self, register: SpecialRegister) -> Result<u32, XtensaError> {
        self.save_register(Register::Cpu(CpuRegister::scratch()))?;

        self.execute_instruction(instruction::rsr(register, CpuRegister::scratch()))?;
        self.xdm.read_ddr()
    }

    fn write_special_register(
        &mut self,
        register: SpecialRegister,
        value: u32,
    ) -> Result<(), XtensaError> {
        self.save_register(Register::Cpu(CpuRegister::scratch()))?;
        self.save_register(Register::Special(register))?;

        self.xdm.write_ddr(value)?;

        // DDR -> scratch
        self.xdm.execute_instruction(instruction::rsr(
            SpecialRegister::Ddr,
            CpuRegister::scratch(),
        ))?;

        // scratch -> target register
        self.xdm
            .execute_instruction(instruction::wsr(register, CpuRegister::scratch()))?;

        Ok(())
    }

    fn write_cpu_register(&mut self, register: CpuRegister, value: u32) -> Result<(), XtensaError> {
        self.save_register(Register::Cpu(register))?;

        self.xdm.write_ddr(value)?;
        self.xdm
            .execute_instruction(instruction::rsr(SpecialRegister::Ddr, register))?;

        Ok(())
    }

    fn debug_execution_error_impl(&mut self, status: XdmError) -> Result<(), XtensaError> {
        if let XdmError::ExecExeception = status {
            if !self.state.print_exception_cause {
                tracing::warn!("Instruction exception while reading previous exception");
                return Ok(());
            }

            tracing::warn!("Failed to execute instruction, attempting to read debug info");

            // clear ExecException to allow new instructions to run
            self.xdm.clear_exec_exception()?;

            for (name, reg) in [
                ("EXCCAUSE", SpecialRegister::ExcCause),
                ("EXCVADDR", SpecialRegister::ExcVaddr),
                ("DEBUGCAUSE", SpecialRegister::DebugCause),
            ] {
                let register = self.read_register(Register::Special(reg))?;

                tracing::info!("{}: {:08x}", name, register);
            }
        }

        Ok(())
    }

    fn debug_execution_error(&mut self, status: XdmError) -> Result<(), XtensaError> {
        self.state.print_exception_cause = false;
        let result = self.debug_execution_error_impl(status);
        self.state.print_exception_cause = true;

        result
    }

    fn execute_instruction(&mut self, inst: u32) -> Result<(), XtensaError> {
        let status = self.xdm.execute_instruction(inst);
        if let Err(XtensaError::XdmError(err)) = status {
            self.debug_execution_error(err)?
        }
        status
    }

    fn read_ddr_and_execute(&mut self) -> Result<u32, XtensaError> {
        let status = self.xdm.read_ddr_and_execute();
        if let Err(XtensaError::XdmError(err)) = status {
            self.debug_execution_error(err)?
        }
        status
    }

    fn is_register_saved(&mut self, register: Register) -> bool {
        if register == Register::Special(SpecialRegister::Ddr) {
            // Avoid saving DDR
            return true;
        }
        self.state
            .saved_registers
            .iter()
            .any(|(reg, _)| *reg == register)
    }

    fn read_register(&mut self, register: Register) -> Result<u32, XtensaError> {
        match register {
            Register::Cpu(register) => self.read_cpu_register(register),
            Register::Special(register) => self.read_special_register(register),
        }
    }

    fn save_register(&mut self, register: Register) -> Result<(), XtensaError> {
        if !self.is_register_saved(register) {
            let value = self.read_register(register)?;
            self.state.saved_registers.push((register, value));
        }

        Ok(())
    }

    fn restore_registers(&mut self) -> Result<(), XtensaError> {
        // Clone the list of saved registers so we can iterate over it, but code may still save
        // new registers. We can't take it otherwise the restore loop would unnecessarily save
        // registers.
        // Currently, restoring registers may only use the scratch register which is already saved
        // if we access special registers. This means the register list won't actually change in the
        // next loop.
        let dirty_regs = self.state.saved_registers.clone();
        let mut restore_scratch = None;

        for (register, value) in dirty_regs.iter().copied() {
            match register {
                Register::Cpu(register) if register == CpuRegister::scratch() => {
                    // We need to handle the scratch register (A3) separately as restoring a special
                    // register will overwrite it.
                    restore_scratch = Some(value);
                }
                Register::Cpu(register) => self.write_cpu_register(register, value)?,
                Register::Special(register) => self.write_special_register(register, value)?,
            }
        }

        if self.state.saved_registers.len() != dirty_regs.len() {
            // The scratch register wasn't saved before, but has to be saved now. This case should
            // not currently be reachable.
            restore_scratch =
                self.state
                    .saved_registers
                    .iter()
                    .copied()
                    .find_map(|(reg, value)| {
                        (reg == Register::Cpu(CpuRegister::scratch())).then_some(value)
                    });
        }

        if let Some(value) = restore_scratch {
            self.write_cpu_register(CpuRegister::scratch(), value)?;
        }

        self.state.saved_registers.clear();

        Ok(())
    }
}

impl MemoryInterface for XtensaCommunicationInterface {
    fn read(&mut self, address: u64, mut dst: &mut [u8]) -> Result<(), crate::Error> {
        if dst.is_empty() {
            return Ok(());
        }

        // Write aligned address to the scratch register
        self.write_cpu_register(CpuRegister::scratch(), address as u32 & !0x3)?;

        // Read from address in the scratch register
        self.execute_instruction(instruction::lddr32_p(CpuRegister::scratch()))?;

        // Let's assume we can just do 32b reads, so let's do some pre-massaging on unaligned reads
        if address % 4 != 0 {
            let word = if dst.len() <= 4 {
                self.xdm.read_ddr()?
            } else {
                self.read_ddr_and_execute()?
            };

            let word = word.to_le_bytes();

            let offset = address as usize % 4;
            let bytes_to_copy = (4 - offset).min(dst.len());

            dst[..bytes_to_copy].copy_from_slice(&word[offset..][..bytes_to_copy]);
            dst = &mut dst[bytes_to_copy..];

            if dst.is_empty() {
                return Ok(());
            }
        }

        while dst.len() > 4 {
            let word = self.read_ddr_and_execute()?.to_le_bytes();
            dst[..4].copy_from_slice(&word);
            dst = &mut dst[4..];
        }

        let remaining_bytes = dst.len();

        let word = self.xdm.read_ddr()?;
        dst.copy_from_slice(&word.to_le_bytes()[..remaining_bytes]);

        Ok(())
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, crate::Error> {
        let mut out = [0; 4];
        self.read(address, &mut out)?;

        Ok(u32::from_le_bytes(out))
    }

    fn supports_native_64bit_access(&mut self) -> bool {
        false
    }

    fn read_word_64(&mut self, address: u64) -> anyhow::Result<u64, crate::Error> {
        let mut out = [0; 8];
        self.read(address, &mut out)?;

        Ok(u64::from_le_bytes(out))
    }

    fn read_word_8(&mut self, address: u64) -> anyhow::Result<u8, crate::Error> {
        let mut out = 0;
        self.read(address, std::slice::from_mut(&mut out))?;
        Ok(out)
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> anyhow::Result<(), crate::Error> {
        let data_8 = unsafe {
            std::slice::from_raw_parts_mut(
                data.as_mut_ptr() as *mut u8,
                data.len() * std::mem::size_of::<u64>(),
            )
        };
        self.read_8(address, data_8)
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> anyhow::Result<(), crate::Error> {
        let data_8 = unsafe {
            std::slice::from_raw_parts_mut(
                data.as_mut_ptr() as *mut u8,
                data.len() * std::mem::size_of::<u32>(),
            )
        };
        self.read_8(address, data_8)
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> anyhow::Result<(), crate::Error> {
        self.read(address, data)
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn supports_8bit_transfers(&self) -> anyhow::Result<bool, crate::Error> {
        todo!()
    }

    fn flush(&mut self) -> anyhow::Result<(), crate::Error> {
        todo!()
    }
}
