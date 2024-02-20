//! Xtensa Debug Module Communication

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::{
    architecture::xtensa::arch::{
        instruction::Instruction, CpuRegister, Register, SpecialRegister,
    },
    probe::{DebugProbeError, DeferredResultIndex, JTAGAccess, Probe},
    BreakpointCause, Error as ProbeRsError, HaltReason, MemoryInterface,
};

use super::xdm::{Error as XdmError, Xdm};

/// Possible Xtensa errors
#[derive(thiserror::Error, Debug)]
pub enum XtensaError {
    /// An error originating from the DebugProbe
    #[error("Debug Probe Error")]
    DebugProbe(#[from] DebugProbeError),
    /// Xtensa debug module error
    #[error("Xtensa debug module error")]
    XdmError(#[from] XdmError),
    /// A timeout occurred
    // TODO: maybe we could be a bit more specific
    #[error("The operation has timed out")]
    Timeout,
    /// The connected target is not an Xtensa device.
    #[error("Connected target is not an Xtensa device.")]
    NoXtensaTarget,
    /// The requested register is not available.
    #[error("The requested register is not available.")]
    RegisterNotAvailable,
    /// The result index of a batched command is not available.
    #[error("The requested data is not available due to a previous error.")]
    BatchedResultNotAvailable,
}

impl From<XtensaError> for DebugProbeError {
    fn from(e: XtensaError) -> DebugProbeError {
        match e {
            XtensaError::DebugProbe(err) => err,
            other_error => DebugProbeError::Other(other_error.into()),
        }
    }
}

impl From<XtensaError> for ProbeRsError {
    fn from(err: XtensaError) -> Self {
        match err {
            XtensaError::DebugProbe(e) => e.into(),
            other => ProbeRsError::Xtensa(other),
        }
    }
}

#[derive(Clone, Copy)]
#[allow(unused)]
pub(super) enum DebugLevel {
    L2 = 2,
    L3 = 3,
    L4 = 4,
    L5 = 5,
    L6 = 6,
    L7 = 7,
}

impl DebugLevel {
    pub fn pc(self) -> SpecialRegister {
        match self {
            DebugLevel::L2 => SpecialRegister::Epc2,
            DebugLevel::L3 => SpecialRegister::Epc3,
            DebugLevel::L4 => SpecialRegister::Epc4,
            DebugLevel::L5 => SpecialRegister::Epc5,
            DebugLevel::L6 => SpecialRegister::Epc6,
            DebugLevel::L7 => SpecialRegister::Epc7,
        }
    }

    pub fn ps(self) -> SpecialRegister {
        match self {
            DebugLevel::L2 => SpecialRegister::Eps2,
            DebugLevel::L3 => SpecialRegister::Eps3,
            DebugLevel::L4 => SpecialRegister::Eps4,
            DebugLevel::L5 => SpecialRegister::Eps5,
            DebugLevel::L6 => SpecialRegister::Eps6,
            DebugLevel::L7 => SpecialRegister::Eps7,
        }
    }
}

struct XtensaCommunicationInterfaceState {
    /// Pairs of (register, deferred value). The value is optional where None means "being restored"
    saved_registers: HashMap<Register, Option<DeferredResultIndex>>,

    is_halted: bool,
}

/// An interface that implements controls for Xtensa cores.
pub struct XtensaCommunicationInterface {
    /// The Xtensa debug module
    pub(super) xdm: Xdm,
    state: XtensaCommunicationInterfaceState,

    hw_breakpoint_num: u32,
    debug_level: DebugLevel,
}

impl XtensaCommunicationInterface {
    /// Create the Xtensa communication interface using the underlying probe driver
    pub fn new(probe: Box<dyn JTAGAccess>) -> Result<Self, (Box<dyn JTAGAccess>, DebugProbeError)> {
        let xdm = Xdm::new(probe).map_err(|(probe, e)| (probe, e.into()))?;

        let mut s = Self {
            xdm,
            state: XtensaCommunicationInterfaceState {
                saved_registers: Default::default(),
                is_halted: false,
            },
            // TODO chip-specific configuration
            hw_breakpoint_num: 2,
            debug_level: DebugLevel::L6,
        };

        match s.init() {
            Ok(()) => Ok(s),

            Err(e) => Err((s.xdm.free(), e.into())),
        }
    }

    /// Destruct the interface and return the stored probe driver.
    pub fn close(self) -> Probe {
        Probe::from_attached_probe(self.xdm.probe.into_probe())
    }

    /// Read the targets IDCODE.
    pub fn read_idcode(&mut self) -> Result<u32, DebugProbeError> {
        Ok(self.xdm.read_idcode()?)
    }

    fn init(&mut self) -> Result<(), XtensaError> {
        // TODO any initialization that needs to be done
        Ok(())
    }

    /// Returns the number of hardware breakpoints the target supports.
    ///
    /// On the Xtensa architecture this is the `NIBREAK` configuration parameter.
    pub fn available_breakpoint_units(&self) -> u32 {
        self.hw_breakpoint_num
    }

    /// Enters OCD mode and halts the core.
    pub fn enter_ocd_mode(&mut self) -> Result<(), XtensaError> {
        self.xdm.halt()?;
        tracing::info!("Entered OCD mode");
        Ok(())
    }

    /// Returns whether the core is in OCD mode.
    pub fn is_in_ocd_mode(&mut self) -> Result<bool, XtensaError> {
        self.xdm.is_in_ocd_mode()
    }

    /// Leaves OCD mode, restores state and resumes program execution.
    pub fn leave_ocd_mode(&mut self) -> Result<(), XtensaError> {
        self.restore_registers()?;
        self.resume()?;
        self.xdm.leave_ocd_mode()?;
        tracing::info!("Left OCD mode");
        Ok(())
    }

    /// Resets the processor core.
    pub fn reset(&mut self) -> Result<(), XtensaError> {
        match self.reset_and_halt(Duration::from_millis(500)) {
            Ok(_) => {
                self.resume()?;

                Ok(())
            }
            Err(error) => Err(XtensaError::DebugProbe(DebugProbeError::Other(
                anyhow::anyhow!("Error during reset").context(error),
            ))),
        }
    }

    /// Resets the processor core and halts it immediately.
    pub fn reset_and_halt(&mut self, timeout: Duration) -> Result<(), XtensaError> {
        self.xdm.target_reset_assert()?;
        self.xdm.halt_on_reset(true);
        self.xdm.target_reset_deassert()?;
        self.wait_for_core_halted(timeout)?;
        self.xdm.halt_on_reset(false);

        // TODO: this is only necessary to run code, so this might not be the best place
        // Make sure the CPU is in a known state and is able to run code we download.
        self.write_register({
            let mut ps = ProgramStatus(0);
            ps.set_intlevel(1);
            ps.set_user_mode(true);
            ps.set_woe(true);
            ps
        })?;

        Ok(())
    }

    /// Halts the core.
    pub fn halt(&mut self) -> Result<(), XtensaError> {
        tracing::debug!("Halting core");
        self.xdm.halt()
    }

    /// Returns whether the core is halted.
    pub fn is_halted(&mut self) -> Result<bool, XtensaError> {
        if !self.state.is_halted {
            self.state.is_halted = self.xdm.is_halted()?;
        }

        Ok(self.state.is_halted)
    }

    /// Waits until the core is halted.
    ///
    /// This function lowers the interrupt level to allow halting on debug exceptions.
    pub fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), XtensaError> {
        let now = Instant::now();
        while !self.is_halted()? {
            if now.elapsed() > timeout {
                tracing::warn!("Timeout waiting for core to halt");
                return Err(XtensaError::Timeout);
            }

            std::thread::sleep(Duration::from_millis(1));
        }
        tracing::debug!("Core halted");

        // Force a low INTLEVEL to allow halting on debug exceptions
        // TODO: do this only if we set a breakpoint or watchpoint or single step
        let mut ps = self.read_register::<ProgramStatus>()?;
        ps.set_intlevel(1);
        self.schedule_write_register(ps)?;

        Ok(())
    }

    /// Steps the core by one instruction.
    pub fn step(&mut self) -> Result<(), XtensaError> {
        self.schedule_write_register(ICountLevel(self.debug_level as u32))?;

        // An exception is generated at the beginning of an instruction that would overflow ICOUNT.
        self.schedule_write_register(ICount(-2_i32 as u32))?;

        self.resume()?;
        self.wait_for_core_halted(Duration::from_millis(100))?;

        // Avoid stopping again
        self.schedule_write_register(ICountLevel(self.debug_level as u32 + 1))?;

        Ok(())
    }

    /// Resumes program execution.
    pub fn resume(&mut self) -> Result<(), XtensaError> {
        tracing::debug!("Resuming core");
        self.state.is_halted = false;
        self.xdm.resume()?;

        Ok(())
    }

    fn schedule_read_cpu_register(&mut self, register: CpuRegister) -> DeferredResultIndex {
        self.xdm
            .schedule_execute_instruction(Instruction::Wsr(SpecialRegister::Ddr, register));
        self.xdm.schedule_read_ddr()
    }

    fn schedule_read_special_register(
        &mut self,
        register: SpecialRegister,
    ) -> Result<DeferredResultIndex, XtensaError> {
        let save_key = self.save_register(CpuRegister::A3)?;

        // Read special register into the scratch register
        self.xdm
            .schedule_execute_instruction(Instruction::Rsr(register, CpuRegister::A3));

        let reader = self.schedule_read_cpu_register(CpuRegister::A3);

        self.restore_register(save_key)?;

        Ok(reader)
    }

    fn schedule_write_special_register(
        &mut self,
        register: SpecialRegister,
        value: u32,
    ) -> Result<(), XtensaError> {
        tracing::debug!("Writing special register: {:?}", register);
        let save_key = self.save_register(CpuRegister::A3)?;

        self.xdm.schedule_write_ddr(value);

        // DDR -> scratch
        self.xdm
            .schedule_execute_instruction(Instruction::Rsr(SpecialRegister::Ddr, CpuRegister::A3));

        // scratch -> target special register
        self.xdm
            .schedule_execute_instruction(Instruction::Wsr(register, CpuRegister::A3));

        self.restore_register(save_key)?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn schedule_write_cpu_register(
        &mut self,
        register: CpuRegister,
        value: u32,
    ) -> Result<(), XtensaError> {
        tracing::debug!("Writing {:x} to register: {:?}", value, register);

        self.xdm.schedule_write_ddr(value);
        self.xdm
            .schedule_execute_instruction(Instruction::Rsr(SpecialRegister::Ddr, register));

        Ok(())
    }

    /// Read a register.
    pub fn read_register<R: TypedRegister>(&mut self) -> Result<R, XtensaError> {
        let value = self.read_register_untyped(R::register())?;

        Ok(R::from_u32(value))
    }

    /// Schedules reading a register.
    pub fn schedule_read_register<R: TypedRegister>(
        &mut self,
    ) -> Result<DeferredResultIndex, XtensaError> {
        self.schedule_read_register_untyped(R::register())
    }

    /// Write a register.
    pub fn write_register<R: TypedRegister>(&mut self, reg: R) -> Result<(), XtensaError> {
        self.write_register_untyped(R::register(), reg.as_u32())?;

        Ok(())
    }

    /// Schedules writing a register.
    pub fn schedule_write_register<R: TypedRegister>(&mut self, reg: R) -> Result<(), XtensaError> {
        self.schedule_write_register_untyped(R::register(), reg.as_u32())?;

        Ok(())
    }

    /// Schedules reading a register.
    pub fn schedule_read_register_untyped(
        &mut self,
        register: impl Into<Register>,
    ) -> Result<DeferredResultIndex, XtensaError> {
        match register.into() {
            Register::Cpu(register) => Ok(self.schedule_read_cpu_register(register)),
            Register::Special(register) => self.schedule_read_special_register(register),
            Register::CurrentPc => self.schedule_read_special_register(self.debug_level.pc()),
            Register::CurrentPs => self.schedule_read_special_register(self.debug_level.ps()),
        }
    }

    /// Read a register.
    pub fn read_register_untyped(
        &mut self,
        register: impl Into<Register>,
    ) -> Result<u32, XtensaError> {
        let reader = self.schedule_read_register_untyped(register)?;
        Ok(self.xdm.read_deferred_result(reader)?.into_u32())
    }

    /// Schedules writing a register.
    pub fn schedule_write_register_untyped(
        &mut self,
        register: impl Into<Register>,
        value: u32,
    ) -> Result<(), XtensaError> {
        match register.into() {
            Register::Cpu(register) => self.schedule_write_cpu_register(register, value),
            Register::Special(register) => self.schedule_write_special_register(register, value),
            Register::CurrentPc => {
                self.schedule_write_special_register(self.debug_level.pc(), value)
            }
            Register::CurrentPs => {
                self.schedule_write_special_register(self.debug_level.ps(), value)
            }
        }
    }

    /// Write a register.
    pub fn write_register_untyped(
        &mut self,
        register: impl Into<Register>,
        value: u32,
    ) -> Result<(), XtensaError> {
        self.schedule_write_register_untyped(register, value)?;
        self.xdm.execute()
    }

    #[tracing::instrument(skip(self, register), fields(register))]
    fn save_register(
        &mut self,
        register: impl Into<Register>,
    ) -> Result<Option<Register>, XtensaError> {
        let register = register.into();

        tracing::Span::current().record("register", &format!("{register:?}"));

        if matches!(
            register,
            Register::Special(
                SpecialRegister::Ddr | SpecialRegister::ICount | SpecialRegister::ICountLevel
            )
        ) {
            // Avoid saving some registers
            return Ok(None);
        }

        let is_saved = self.state.saved_registers.contains_key(&register);

        if is_saved {
            return Ok(None);
        }

        tracing::debug!("Saving register: {:?}", register);
        let value = self.schedule_read_register_untyped(register)?;
        self.state.saved_registers.insert(register, Some(value));

        Ok(Some(register))
    }

    #[tracing::instrument(skip(self))]
    fn restore_register(&mut self, key: Option<Register>) -> Result<(), XtensaError> {
        let Some(key) = key else {
            return Ok(());
        };

        tracing::debug!("Restoring register: {:?}", key);

        // Remove the result early, so an error here will not cause a panic in `restore_registers`.
        if let Some(value) = self.state.saved_registers.remove(&key) {
            let reader = value.unwrap();
            let value = self.xdm.read_deferred_result(reader)?.into_u32();

            self.write_register_untyped(key, value)?;
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn restore_registers(&mut self) -> Result<(), XtensaError> {
        tracing::debug!("Restoring registers");

        // Clone the list of saved registers so we can iterate over it, but code may still save
        // new registers. We can't take it otherwise the restore loop would unnecessarily save
        // registers.
        // Currently, restoring registers may only use the scratch register which is already saved
        // if we access special registers. This means the register list won't actually change in the
        // next loop.
        let dirty_regs = self
            .state
            .saved_registers
            .keys()
            .copied()
            .collect::<Vec<_>>();

        let dirty_count = dirty_regs.len();

        let mut restore_scratch = None;

        for register in dirty_regs {
            let reader = self
                .state
                .saved_registers
                .get_mut(&register)
                .unwrap()
                .take()
                .unwrap_or_else(|| {
                    panic!(
                        "Failed to get original value of dirty register {:?}. This is a bug.",
                        register
                    )
                });
            let value = self.xdm.read_deferred_result(reader)?.into_u32();

            if register == Register::Cpu(CpuRegister::A3) {
                // We need to handle the scratch register (A3) separately as restoring a special
                // register will overwrite it.
                restore_scratch = Some(value);
            } else {
                self.schedule_write_register_untyped(register, value)?;
            }
        }

        if self.state.saved_registers.len() != dirty_count {
            // The scratch register wasn't saved before, but has to be restored now. This case should
            // not currently be reachable.
            // TODO: we shouldn't special-case the A3 register, I think
            if let Some(reader) = self
                .state
                .saved_registers
                .get_mut(&Register::Cpu(CpuRegister::A3))
            {
                if let Some(reader) = reader.take() {
                    let value = self.xdm.read_deferred_result(reader)?.into_u32();

                    restore_scratch = Some(value);
                }
            }
        }

        if let Some(value) = restore_scratch {
            self.schedule_write_register_untyped(CpuRegister::A3, value)?;
        }

        self.state.saved_registers.clear();
        self.xdm.execute()
    }

    fn read_memory(&mut self, address: u64, mut dst: &mut [u8]) -> Result<(), XtensaError> {
        tracing::debug!("Reading {} bytes from address {:08x}", dst.len(), address);
        if dst.is_empty() {
            return Ok(());
        }

        let was_halted = self.is_halted()?;
        if !was_halted {
            self.xdm.schedule_halt();
            self.wait_for_core_halted(Duration::from_millis(100))?;
        }

        // Write aligned address to the scratch register
        let key = self.save_register(CpuRegister::A3)?;
        self.schedule_write_cpu_register(CpuRegister::A3, address as u32 & !0x3)?;

        // Read from address in the scratch register
        self.xdm
            .schedule_execute_instruction(Instruction::Lddr32P(CpuRegister::A3));

        let mut to_read = dst.len();

        // Let's assume we can just do 32b reads, so let's do some pre-massaging on unaligned reads
        let first_read = if address % 4 != 0 {
            let offset = address as usize % 4;

            // Avoid executing another read if we only have to read a single word
            let first_read = if offset + to_read <= 4 {
                self.xdm.schedule_read_ddr()
            } else {
                self.xdm.schedule_read_ddr_and_execute()
            };

            let bytes_to_copy = (4 - offset).min(to_read);

            to_read -= bytes_to_copy;

            Some((first_read, offset, bytes_to_copy))
        } else {
            None
        };

        let mut aligned_reads = vec![];
        if to_read > 0 {
            let words = if to_read % 4 == 0 {
                to_read / 4
            } else {
                to_read / 4 + 1
            };

            for _ in 0..words - 1 {
                aligned_reads.push(self.xdm.schedule_read_ddr_and_execute());
            }
            aligned_reads.push(self.xdm.schedule_read_ddr());
        };

        if let Some((read, offset, bytes_to_copy)) = first_read {
            let word = self
                .xdm
                .read_deferred_result(read)?
                .into_u32()
                .to_le_bytes();

            dst[..bytes_to_copy].copy_from_slice(&word[offset..][..bytes_to_copy]);
            dst = &mut dst[bytes_to_copy..];
        }

        for read in aligned_reads {
            let word = self
                .xdm
                .read_deferred_result(read)?
                .into_u32()
                .to_le_bytes();

            let bytes = dst.len().min(4);

            dst[..bytes].copy_from_slice(&word[..bytes]);
            dst = &mut dst[bytes..];
        }

        self.restore_register(key)?;

        if !was_halted {
            self.resume()?;
        }

        Ok(())
    }

    fn write_memory_unaligned8(&mut self, address: u32, data: &[u8]) -> Result<(), XtensaError> {
        if data.is_empty() {
            return Ok(());
        }

        let key = self.save_register(CpuRegister::A3)?;

        let offset = address as usize % 4;
        let aligned_address = address & !0x3;

        assert!(
            offset + data.len() <= 4,
            "Trying to write data crossing a word boundary"
        );

        // Avoid reading back if we have a complete word
        let data = if offset == 0 && data.len() == 4 {
            data.try_into().unwrap()
        } else {
            // Read the aligned word
            let mut word = [0; 4];
            self.read_memory(aligned_address as u64, &mut word)?;

            // Replace the written bytes. This will also panic if the input is crossing a word boundary
            word[offset..][..data.len()].copy_from_slice(data);

            word
        };

        // Write the word back
        self.schedule_write_register_untyped(CpuRegister::A3, aligned_address)?;
        self.xdm.schedule_write_ddr(u32::from_le_bytes(data));
        self.xdm
            .schedule_execute_instruction(Instruction::Sddr32P(CpuRegister::A3));
        self.restore_register(key)?;

        self.xdm.execute()
    }

    fn write_memory(&mut self, address: u64, data: &[u8]) -> Result<(), XtensaError> {
        tracing::debug!("Writing {} bytes to address {:08x}", data.len(), address);
        if data.is_empty() {
            return Ok(());
        }

        let was_halted = self.is_halted()?;
        if !was_halted {
            self.xdm.schedule_halt();
            self.wait_for_core_halted(Duration::from_millis(100))?;
        }

        let key = self.save_register(CpuRegister::A3)?;

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
            self.schedule_write_register_untyped(CpuRegister::A3, addr)?;

            self.xdm
                .schedule_write_instruction(Instruction::Sddr32P(CpuRegister::A3));

            while buffer.len() > 4 {
                let mut word = [0; 4];
                word[..].copy_from_slice(&buffer[..4]);
                let word = u32::from_le_bytes(word);

                // Write data to DDR and store
                self.xdm.schedule_write_ddr_and_execute(word);

                buffer = &buffer[4..];
                addr += 4;
            }
        }

        // We store the narrow tail of the data separately
        if !buffer.is_empty() {
            self.write_memory_unaligned8(addr, buffer)?;
        }

        self.restore_register(key)?;

        self.xdm.execute()?;

        if !was_halted {
            self.resume()?;
        }

        // TODO: implement cache flushing on CPUs that need it.

        Ok(())
    }
}

/// DataType
///
/// # Safety
/// Don't implement this trait
unsafe trait DataType: Sized {}
unsafe impl DataType for u8 {}
unsafe impl DataType for u16 {}
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
    fn read(&mut self, address: u64, dst: &mut [u8]) -> Result<(), crate::Error> {
        self.read_memory(address, dst)?;

        Ok(())
    }

    fn supports_native_64bit_access(&mut self) -> bool {
        false
    }

    fn read_word_64(&mut self, address: u64) -> anyhow::Result<u64, crate::Error> {
        let mut out = [0; 8];
        self.read(address, &mut out)?;

        Ok(u64::from_le_bytes(out))
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, crate::Error> {
        let mut out = [0; 4];
        self.read(address, &mut out)?;

        Ok(u32::from_le_bytes(out))
    }

    fn read_word_16(&mut self, address: u64) -> Result<u16, crate::Error> {
        let mut out = [0; 2];
        self.read(address, &mut out)?;

        Ok(u16::from_le_bytes(out))
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

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> anyhow::Result<(), crate::Error> {
        self.read_8(address, as_bytes_mut(data))
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> anyhow::Result<(), crate::Error> {
        self.read(address, data)
    }

    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), crate::Error> {
        self.write_memory(address, data)?;

        Ok(())
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> anyhow::Result<(), crate::Error> {
        self.write(address, &data.to_le_bytes())
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> anyhow::Result<(), crate::Error> {
        self.write(address, &data.to_le_bytes())
    }

    fn write_word_16(&mut self, address: u64, data: u16) -> anyhow::Result<(), crate::Error> {
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

    fn write_16(&mut self, address: u64, data: &[u16]) -> anyhow::Result<(), crate::Error> {
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

/// An Xtensa core register
pub trait TypedRegister: Copy {
    /// Returns the register ID.
    fn register() -> Register;

    /// Creates a new register from the given value.
    fn from_u32(value: u32) -> Self;

    /// Returns the register value.
    fn as_u32(self) -> u32;
}

macro_rules! u32_register {
    ($name:ident, $register:expr) => {
        impl TypedRegister for $name {
            fn register() -> Register {
                Register::from($register)
            }

            fn from_u32(value: u32) -> Self {
                Self(value)
            }

            fn as_u32(self) -> u32 {
                self.0
            }
        }
    };
}

bitfield::bitfield! {
    /// The `DEBUGCAUSE` register.
    #[derive(Copy, Clone)]
    pub struct DebugCause(u32);
    impl Debug;

    /// Instruction counter exception
    pub icount_exception,    set_icount_exception   : 0;

    /// Instruction breakpoint exception
    pub ibreak_exception,    set_ibreak_exception   : 1;

    /// Data breakpoint (watchpoint) exception
    pub dbreak_exception,    set_dbreak_exception   : 2;

    /// Break instruction exception
    pub break_instruction,   set_break_instruction  : 3;

    /// Narrow Break instruction exception
    pub break_n_instruction, set_break_n_instruction: 4;

    /// Debug interrupt exception
    pub debug_interrupt,     set_debug_interrupt    : 5;

    /// Data breakpoint number
    pub dbreak_num,          set_dbreak_num         : 11, 8;
}
u32_register!(DebugCause, SpecialRegister::DebugCause);

impl DebugCause {
    /// Returns the reason why the core is halted.
    pub fn halt_reason(&self) -> HaltReason {
        let is_icount_exception = self.icount_exception();
        let is_ibreak_exception = self.ibreak_exception();
        let is_break_instruction = self.break_instruction();
        let is_break_n_instruction = self.break_n_instruction();
        let is_dbreak_exception = self.dbreak_exception();
        let is_debug_interrupt = self.debug_interrupt();

        let is_breakpoint = is_break_instruction || is_break_n_instruction;

        let count = is_icount_exception as u8
            + is_ibreak_exception as u8
            + is_break_instruction as u8
            + is_break_n_instruction as u8
            + is_dbreak_exception as u8
            + is_debug_interrupt as u8;

        if count > 1 {
            tracing::debug!("DebugCause: {:?}", self);

            // We cannot identify why the chip halted,
            // it could be for multiple reasons.

            // For debuggers, it's important to know if
            // the core halted because of a breakpoint.
            // Because of this, we still return breakpoint
            // even if other reasons are possible as well.
            if is_breakpoint {
                HaltReason::Breakpoint(BreakpointCause::Unknown)
            } else {
                HaltReason::Multiple
            }
        } else if is_icount_exception {
            HaltReason::Step
        } else if is_ibreak_exception {
            HaltReason::Breakpoint(BreakpointCause::Hardware)
        } else if is_breakpoint {
            HaltReason::Breakpoint(BreakpointCause::Software)
        } else if is_dbreak_exception {
            HaltReason::Watchpoint
        } else if is_debug_interrupt {
            HaltReason::Request
        } else {
            HaltReason::Unknown
        }
    }
}

bitfield::bitfield! {
    /// The `PS` (Program Status) register.
    ///
    /// The physical register depends on the debug level.
    #[derive(Copy, Clone)]
    pub struct ProgramStatus(u32);
    impl Debug;

    /// Interrupt level disable
    pub intlevel,  set_intlevel : 3, 0;

    /// Exception mode
    pub excm,      set_excm     : 4;

    /// User mode
    pub user_mode, set_user_mode: 5;

    /// Privilege level (when using the MMU option)
    pub ring,      set_ring     : 7, 6;

    /// Old window base
    pub owb,       set_owb      : 11, 8;

    /// Call increment
    pub callinc,   set_callinc  : 17, 16;

    /// Window overflow-detection enable
    pub woe,       set_woe      : 18;
}
u32_register!(ProgramStatus, Register::CurrentPs);

/// The `IBREAKEN` (Instruction Breakpoint Enable) register.
#[derive(Copy, Clone, Debug)]
pub struct IBreakEn(pub u32);
u32_register!(IBreakEn, SpecialRegister::IBreakEnable);

/// The `ICOUNT` (Instruction Counter) register.
#[derive(Copy, Clone, Debug)]
pub struct ICount(pub u32);
u32_register!(ICount, SpecialRegister::ICount);

/// The `ICOUNTLEVEL` (Instruction Count Level) register.
#[derive(Copy, Clone, Debug)]
pub struct ICountLevel(pub u32);
u32_register!(ICountLevel, SpecialRegister::ICountLevel);
