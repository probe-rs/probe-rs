//! Xtensa Debug Module Communication

// TODO: remove
#![allow(missing_docs)]

use std::collections::HashMap;

use crate::{
    architecture::xtensa::arch::{
        instruction::Instruction, CacheConfig, ChipConfig, CpuRegister, Register, SpecialRegister,
    },
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
    /// Pairs of (register, value).
    saved_registers: HashMap<Register, u32>,

    print_exception_cause: bool,

    config: ChipConfig,
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
                saved_registers: HashMap::new(),
                print_exception_cause: true,
                config: ChipConfig {
                    // ESP32-S3, TODO inject from the outside
                    icache: CacheConfig::not_present(),
                    dcache: CacheConfig::not_present(),
                },
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
        self.execute_instruction(Instruction::Wsr(SpecialRegister::Ddr, register))?;
        self.xdm.read_ddr()
    }

    fn read_special_register(&mut self, register: SpecialRegister) -> Result<u32, XtensaError> {
        self.save_register(Register::Cpu(CpuRegister::scratch()))?;

        // Read special register into the scratch register
        self.execute_instruction(Instruction::Rsr(register, CpuRegister::scratch()))?;

        // Write the scratch register into DDR
        self.execute_instruction(Instruction::Wsr(
            SpecialRegister::Ddr,
            CpuRegister::scratch(),
        ))?;

        // Read DDR
        self.xdm.read_ddr()
    }

    fn write_special_register(
        &mut self,
        register: SpecialRegister,
        value: u32,
    ) -> Result<(), XtensaError> {
        self.save_register(Register::Cpu(CpuRegister::scratch()))?;
        self.save_register(Register::Special(register))?;

        tracing::debug!("Writing special register: {:?}", register);

        self.xdm.write_ddr(value)?;

        // DDR -> scratch
        self.xdm.execute_instruction(Instruction::Rsr(
            SpecialRegister::Ddr,
            CpuRegister::scratch(),
        ))?;

        // scratch -> target special register
        self.xdm
            .execute_instruction(Instruction::Wsr(register, CpuRegister::scratch()))?;

        Ok(())
    }

    fn write_cpu_register(&mut self, register: CpuRegister, value: u32) -> Result<(), XtensaError> {
        self.save_register(Register::Cpu(register))?;

        tracing::debug!("Writing register: {:?}", register);

        self.xdm.write_ddr(value)?;
        self.xdm
            .execute_instruction(Instruction::Rsr(SpecialRegister::Ddr, register))?;

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

    fn execute_instruction(&mut self, inst: Instruction) -> Result<(), XtensaError> {
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

    fn write_ddr_and_execute(&mut self, value: u32) -> Result<(), XtensaError> {
        let status = self.xdm.write_ddr_and_execute(value);
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
        self.state.saved_registers.contains_key(&register)
    }

    fn read_register(&mut self, register: Register) -> Result<u32, XtensaError> {
        match register {
            Register::Cpu(register) => self.read_cpu_register(register),
            Register::Special(register) => self.read_special_register(register),
        }
    }

    fn save_register(&mut self, register: Register) -> Result<(), XtensaError> {
        if !self.is_register_saved(register) {
            tracing::debug!("Saving register: {:?}", register);
            let value = self.read_register(register)?;
            self.state.saved_registers.insert(register, value);
        }

        Ok(())
    }

    fn restore_registers(&mut self) -> Result<(), XtensaError> {
        tracing::debug!("Restoring registers");

        // Clone the list of saved registers so we can iterate over it, but code may still save
        // new registers. We can't take it otherwise the restore loop would unnecessarily save
        // registers.
        // Currently, restoring registers may only use the scratch register which is already saved
        // if we access special registers. This means the register list won't actually change in the
        // next loop.
        let dirty_regs = self.state.saved_registers.clone();
        let mut restore_scratch = None;

        for (register, value) in dirty_regs.iter().map(|(k, v)| (*k, *v)) {
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
            restore_scratch = self
                .state
                .saved_registers
                .get(&Register::Cpu(CpuRegister::scratch()))
                .copied();
        }

        if let Some(value) = restore_scratch {
            self.write_cpu_register(CpuRegister::scratch(), value)?;
        }

        self.state.saved_registers.clear();

        Ok(())
    }

    fn write_memory_unaligned8(&mut self, address: u32, data: &[u8]) -> Result<(), crate::Error> {
        if data.is_empty() {
            return Ok(());
        }

        let offset = address as usize % 4;
        let aligned_address = address & !0x3;

        // Read the aligned word
        let mut word = [0; 4];
        self.read(aligned_address as u64, &mut word)?;

        // Replace the written bytes. This will also panic if the input is crossing a word boundary
        word[offset..][..data.len()].copy_from_slice(data);

        // Write the word back
        self.write_cpu_register(CpuRegister::A3, aligned_address)?;
        self.xdm.write_ddr(u32::from_le_bytes(word))?;
        self.execute_instruction(Instruction::Sddr32P(CpuRegister::A3))?;

        Ok(())
    }
}

/// DataType
///
/// # Safety
/// Don't implement this trait
unsafe trait DataType: Sized {}
unsafe impl DataType for u8 {}
unsafe impl DataType for u32 {}
unsafe impl DataType for u64 {}

fn as_bytes<T: DataType>(data: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *mut u8, std::mem::size_of_val(data)) }
}

fn as_bytes_mut<T: DataType>(data: &mut [T]) -> &mut [u8] {
    unsafe {
        std::slice::from_raw_parts_mut(data.as_mut_ptr() as *mut u8, std::mem::size_of_val(data))
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
        self.execute_instruction(Instruction::Lddr32P(CpuRegister::scratch()))?;

        // Let's assume we can just do 32b reads, so let's do some pre-massaging on unaligned reads
        if address % 4 != 0 {
            let offset = address as usize % 4;

            // Avoid executing another read if we only have to read a single word
            let word = if offset + dst.len() <= 4 {
                self.xdm.read_ddr()?
            } else {
                self.read_ddr_and_execute()?
            };

            let word = word.to_le_bytes();

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
        self.read_8(address, as_bytes_mut(data))
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> anyhow::Result<(), crate::Error> {
        self.read_8(address, as_bytes_mut(data))
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> anyhow::Result<(), crate::Error> {
        self.read(address, data)
    }

    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), crate::Error> {
        if data.is_empty() {
            return Ok(());
        }

        let address = address as u32;

        let mut addr = address;
        let mut buffer = data;

        // We store the unaligned head of the data separately
        if addr % 4 != 0 {
            let unaligned_bytes = (4 - (addr % 4) as usize).min(buffer.len());

            self.write_memory_unaligned8(addr, &buffer[..unaligned_bytes])?;

            buffer = &buffer[unaligned_bytes..];
            addr += unaligned_bytes as u32;
        }

        if buffer.len() > 4 {
            // Prepare store instruction
            self.write_cpu_register(CpuRegister::A3, addr)?;
            self.xdm
                .write_instruction(Instruction::Sddr32P(CpuRegister::A3))?;

            while buffer.len() > 4 {
                let mut word = [0; 4];
                word[..].copy_from_slice(&buffer[..4]);
                let word = u32::from_le_bytes(word);

                // Write data to DDR and store
                self.write_ddr_and_execute(word)?;

                buffer = &buffer[4..];
                addr += 4;
            }
        }

        // We store the narrow tail of the data separately
        if !buffer.is_empty() {
            self.write_memory_unaligned8(addr, buffer)?;
        }

        // flush caches if we need to
        let flush_icache = self.state.config.icache.contains(address as u32);
        let flush_dcache = self.state.config.dcache.contains(address as u32);

        let line_size = match (flush_icache, flush_dcache) {
            (true, true) => self
                .state
                .config
                .icache
                .line_size
                .max(self.state.config.dcache.line_size),
            (true, false) => self.state.config.icache.line_size,
            (false, true) => self.state.config.dcache.line_size,
            (false, false) => {
                // Written memory can't be cached, so we don't need to flush
                return Ok(());
            }
        };

        let mut off = 0;
        let mut addr = address & !0x03;
        let addr_end = addr + data.len() as u32;

        while addr + off < addr_end {
            if off == 0 {
                self.write_cpu_register(CpuRegister::A3, addr)?;
            }

            if flush_icache {
                self.execute_instruction(Instruction::Ihi(CpuRegister::A3, off))?;
            }
            if flush_dcache {
                self.execute_instruction(Instruction::Dhwbi(CpuRegister::A3, off))?;
            }

            off += line_size;

            if off > 1020 {
                addr += off;
                off = 0;
            }
        }

        Ok(())
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> anyhow::Result<(), crate::Error> {
        self.write(address, &data.to_le_bytes())
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> anyhow::Result<(), crate::Error> {
        self.write(address, &data.to_le_bytes())
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> anyhow::Result<(), crate::Error> {
        self.write(address, &[data])
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> anyhow::Result<(), crate::Error> {
        self.write_8(address, as_bytes(data))
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> anyhow::Result<(), crate::Error> {
        self.write_8(address, as_bytes(data))
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> anyhow::Result<(), crate::Error> {
        self.write(address, data)
    }

    fn supports_8bit_transfers(&self) -> anyhow::Result<bool, crate::Error> {
        Ok(true)
    }

    fn flush(&mut self) -> anyhow::Result<(), crate::Error> {
        Ok(())
    }
}
