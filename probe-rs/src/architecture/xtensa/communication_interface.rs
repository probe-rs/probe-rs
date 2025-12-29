//! Xtensa Debug Module Communication

use std::{
    collections::HashMap,
    ops::Range,
    time::{Duration, Instant},
};

use zerocopy::IntoBytes;

use crate::{
    BreakpointCause, Error as ProbeRsError, HaltReason, MemoryInterface,
    architecture::xtensa::{
        arch::{CpuRegister, Register, SpecialRegister, instruction::Instruction},
        register_cache::RegisterCache,
        xdm::{DebugStatus, XdmState},
    },
    probe::{DebugProbeError, DeferredResultIndex, JtagAccess},
};

use super::xdm::{Error as XdmError, Xdm};

/// Possible Xtensa errors
#[derive(thiserror::Error, Debug, docsplay::Display)]
pub enum XtensaError {
    /// An error originating from the DebugProbe occurred.
    DebugProbe(#[from] DebugProbeError),

    /// Xtensa debug module error.
    XdmError(#[from] XdmError),

    /// The core is not enabled.
    CoreDisabled,

    /// The operation has timed out.
    // TODO: maybe we could be a bit more specific
    Timeout,

    /// The connected target is not an Xtensa device.
    NoXtensaTarget,

    /// The requested register is not available.
    RegisterNotAvailable,

    /// The result index of a batched command is not available.
    BatchedResultNotAvailable,
}

impl From<XtensaError> for ProbeRsError {
    fn from(err: XtensaError) -> Self {
        match err {
            XtensaError::DebugProbe(e) => e.into(),
            other => ProbeRsError::Xtensa(other),
        }
    }
}

/// Debug interrupt level values.
#[derive(Clone, Copy)]
pub enum DebugLevel {
    /// The CPU was configured to take Debug interrupts at level 2.
    L2 = 2,
    /// The CPU was configured to take Debug interrupts at level 3.
    L3 = 3,
    /// The CPU was configured to take Debug interrupts at level 4.
    L4 = 4,
    /// The CPU was configured to take Debug interrupts at level 5.
    L5 = 5,
    /// The CPU was configured to take Debug interrupts at level 6.
    L6 = 6,
    /// The CPU was configured to take Debug interrupts at level 7.
    L7 = 7,
}

impl DebugLevel {
    /// The register that contains the current program counter value.
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

    /// The register that contains the current program status value.
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

/// Xtensa interface state.
#[derive(Default)]
pub(super) struct XtensaInterfaceState {
    /// The register cache.
    pub(super) register_cache: RegisterCache,

    /// Whether the core is halted.
    // This roughly relates to Core Debug States (true = Running, false = [Stopped, Stepping])
    pub(super) is_halted: bool,
}

/// Properties of a memory region.
#[derive(Clone, Copy, Default)]
pub struct MemoryRegionProperties {
    /// Whether the CPU supports unaligned stores. (Hardware Alignment Option)
    pub unaligned_store: bool,

    /// Whether the CPU supports unaligned loads. (Hardware Alignment Option)
    pub unaligned_load: bool,

    /// Whether the CPU supports fast memory access in this region. (LDDR32.P/SDDR32.P instructions)
    pub fast_memory_access: bool,
}

/// Properties of an Xtensa CPU core.
pub struct XtensaCoreProperties {
    /// The number of hardware breakpoints the target supports. CPU-specific configuration value.
    pub hw_breakpoint_num: u32,

    /// The interrupt level at which debug exceptions are generated. CPU-specific configuration value.
    pub debug_level: DebugLevel,

    /// Known memory ranges with special properties.
    pub memory_ranges: HashMap<Range<u64>, MemoryRegionProperties>,

    /// Configurable options in the Windowed Register Option
    pub window_option_properties: WindowProperties,
}

impl Default for XtensaCoreProperties {
    fn default() -> Self {
        Self {
            hw_breakpoint_num: 2,
            debug_level: DebugLevel::L6,
            memory_ranges: HashMap::new(),
            window_option_properties: WindowProperties::lx(64),
        }
    }
}

impl XtensaCoreProperties {
    /// Returns the memory range for the given address.
    pub fn memory_properties_at(&self, address: u64) -> MemoryRegionProperties {
        self.memory_ranges
            .iter()
            .find(|(range, _)| range.contains(&address))
            .map(|(_, region)| *region)
            .unwrap_or_default()
    }

    /// Returns the conservative memory range properties for the given address range.
    pub fn memory_range_properties(&self, range: Range<u64>) -> MemoryRegionProperties {
        let mut start = range.start;
        let end = range.end;

        if start == end {
            return MemoryRegionProperties::default();
        }

        let mut properties = MemoryRegionProperties {
            unaligned_store: true,
            unaligned_load: true,
            fast_memory_access: true,
        };
        while start < end {
            // Find region that contains the start address.
            let containing_region = self
                .memory_ranges
                .iter()
                .find(|(range, _)| range.contains(&start));

            let Some((range, region_properties)) = containing_region else {
                // no point in continuing
                return MemoryRegionProperties::default();
            };

            properties.unaligned_store &= region_properties.unaligned_store;
            properties.unaligned_load &= region_properties.unaligned_load;
            properties.fast_memory_access &= region_properties.fast_memory_access;

            // Move start to the end of the region.
            start = range.end;
        }

        properties
    }
}

/// Properties of the windowed register file.
#[derive(Clone, Copy, Debug)]
pub struct WindowProperties {
    /// Whether the CPU has windowed registers.
    pub has_windowed_registers: bool,

    /// The total number of AR registers in the register file.
    pub num_aregs: u8,

    /// The number of registers in a single window.
    pub window_regs: u8,

    /// The number of registers rotated by an LSB of the ROTW instruction.
    pub rotw_rotates: u8,
}

impl WindowProperties {
    /// Create a new WindowProperties instance with the given number of AR registers.
    pub fn lx(num_aregs: u8) -> Self {
        Self {
            has_windowed_registers: true,
            num_aregs,
            window_regs: 16,
            rotw_rotates: 4,
        }
    }

    /// Returns the number of different valid WindowBase values.
    pub fn windowbase_size(&self) -> u8 {
        self.num_aregs / self.rotw_rotates
    }
}

/// Debug module and transport state.
#[derive(Default)]
pub struct XtensaDebugInterfaceState {
    interface_state: XtensaInterfaceState,
    core_properties: XtensaCoreProperties,
    xdm_state: XdmState,
}

/// The higher level of the XDM functionality.
// TODO: this includes core state and CPU configuration that don't exactly belong
// here but one layer up.
pub struct XtensaCommunicationInterface<'probe> {
    /// The Xtensa debug module
    pub(crate) xdm: Xdm<'probe>,
    pub(super) state: &'probe mut XtensaInterfaceState,
    core_properties: &'probe mut XtensaCoreProperties,
}

impl<'probe> XtensaCommunicationInterface<'probe> {
    /// Create the Xtensa communication interface using the underlying probe driver
    pub fn new(
        probe: &'probe mut dyn JtagAccess,
        state: &'probe mut XtensaDebugInterfaceState,
    ) -> Self {
        let XtensaDebugInterfaceState {
            interface_state,
            core_properties,
            xdm_state,
        } = state;
        let xdm = Xdm::new(probe, xdm_state);

        Self {
            xdm,
            state: interface_state,
            core_properties,
        }
    }

    /// Access the properties of the CPU core.
    pub fn core_properties(&mut self) -> &mut XtensaCoreProperties {
        self.core_properties
    }

    /// Read the targets IDCODE.
    pub fn read_idcode(&mut self) -> Result<u32, XtensaError> {
        self.xdm.read_idcode()
    }

    /// Enter debug mode.
    pub fn enter_debug_mode(&mut self) -> Result<(), XtensaError> {
        self.xdm.enter_debug_mode()?;

        self.state.is_halted = self.xdm.status()?.stopped();

        Ok(())
    }

    pub(crate) fn leave_debug_mode(&mut self) -> Result<(), XtensaError> {
        if self.xdm.status()?.stopped() {
            self.restore_registers()?;
            self.resume_core()?;
        }
        self.xdm.leave_ocd_mode()?;

        tracing::debug!("Left OCD mode");

        Ok(())
    }

    /// Returns the number of hardware breakpoints the target supports.
    ///
    /// On the Xtensa architecture this is the `NIBREAK` configuration parameter.
    pub fn available_breakpoint_units(&self) -> u32 {
        self.core_properties.hw_breakpoint_num
    }

    /// Returns whether the core is halted.
    pub fn core_halted(&mut self) -> Result<bool, XtensaError> {
        if !self.state.is_halted {
            self.state.is_halted = self.xdm.status()?.stopped();
        }

        Ok(self.state.is_halted)
    }

    /// Waits until the core is halted.
    ///
    /// This function lowers the interrupt level to allow halting on debug exceptions.
    pub fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), XtensaError> {
        // Wait until halted state is active again.
        let start = Instant::now();

        while !self.core_halted()? {
            if start.elapsed() >= timeout {
                return Err(XtensaError::Timeout);
            }
            // Wait a bit before polling again.
            std::thread::sleep(Duration::from_millis(1));
        }

        Ok(())
    }

    /// Halts the core.
    pub(crate) fn halt(&mut self, timeout: Duration) -> Result<(), XtensaError> {
        self.xdm.schedule_halt();
        self.wait_for_core_halted(timeout)?;
        Ok(())
    }

    /// Halts the core and returns `true` if the core was running before the halt.
    pub(crate) fn halt_with_previous(&mut self, timeout: Duration) -> Result<bool, XtensaError> {
        let was_running = if self.state.is_halted {
            // Core is already halted, we don't need to do anything.
            false
        } else {
            // If we have not halted the core, it may still be halted on a breakpoint, for example.
            // Let's check status.
            let status_idx = self.xdm.schedule_read_nexus_register::<DebugStatus>();
            self.halt(timeout)?;
            let before_status = DebugStatus(self.xdm.read_deferred_result(status_idx)?.into_u32());

            !before_status.stopped()
        };

        Ok(was_running)
    }

    fn fast_halted_access(
        &mut self,
        mut op: impl FnMut(&mut Self) -> Result<(), XtensaError>,
    ) -> Result<(), XtensaError> {
        if self.state.is_halted {
            // Core is already halted, we don't need to do anything.
            return op(self);
        }

        // If we have not halted the core, it may still be halted on a breakpoint, for example.
        // Let's check status.
        let status_idx = self.xdm.schedule_read_nexus_register::<DebugStatus>();

        // Queue up halting.
        self.xdm.schedule_halt();

        // We will need to check if we managed to halt the core.
        let is_halted_idx = self.xdm.schedule_read_nexus_register::<DebugStatus>();
        self.state.is_halted = true;

        // Execute the operation while the core is presumed halted. If it is not, we will have
        // various errors, but we will retry the operation.
        let result = op(self);

        // If the core was running, resume it.
        let before_status = DebugStatus(self.xdm.read_deferred_result(status_idx)?.into_u32());
        if !before_status.stopped() {
            self.resume_core()?;
        }

        // If we did not manage to halt the core at once, let's retry using the slow path.
        let after_status = DebugStatus(self.xdm.read_deferred_result(is_halted_idx)?.into_u32());

        if after_status.stopped() {
            return result;
        }
        self.state.is_halted = false;
        self.halted_access(|this| op(this))
    }

    /// Executes a closure while ensuring the core is halted.
    pub fn halted_access<R>(
        &mut self,
        op: impl FnOnce(&mut Self) -> Result<R, XtensaError>,
    ) -> Result<R, XtensaError> {
        let was_running = self.halt_with_previous(Duration::from_millis(100))?;

        let result = op(self);

        if was_running {
            self.resume_core()?;
        }

        result
    }

    /// Steps the core by one instruction.
    pub fn step(&mut self, by: u32, intlevel: u32) -> Result<(), XtensaError> {
        // Instructions executed below icountlevel increment the ICOUNT register.
        self.schedule_write_register(ICountLevel(intlevel + 1))?;

        // An exception is generated at the beginning of an instruction that would overflow ICOUNT.
        self.schedule_write_register(ICount(-((1 + by) as i32) as u32))?;

        self.resume_core()?;
        // TODO: instructions like WAITI should be emulated as they are not single steppable.
        // For now it's good enough to force a halt on timeout (instead of crashing) although it can
        // stop in a long-running interrupt handler which isn't necessarily what the user wants.
        // Even then, WAITI should be detected and emulated.
        match self.wait_for_core_halted(Duration::from_millis(100)) {
            Ok(()) => {}
            Err(XtensaError::Timeout) => self.halt(Duration::from_millis(100))?,
            Err(e) => return Err(e),
        }

        // Avoid stopping again
        self.schedule_write_register(ICountLevel(0))?;

        Ok(())
    }

    /// Resumes program execution.
    pub fn resume_core(&mut self) -> Result<(), XtensaError> {
        // Any time we resume the core, we need to restore the registers so the the program
        // doesn't crash.
        self.restore_registers()?;
        // We also need to clear the register cache, as the CPU will likely change the registers.
        self.clear_register_cache();

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
        self.ensure_register_saved(CpuRegister::A3)?;
        self.state.register_cache.mark_dirty(CpuRegister::A3.into());

        // Read special register into the scratch register
        self.xdm
            .schedule_execute_instruction(Instruction::Rsr(register, CpuRegister::A3));

        Ok(self.schedule_read_cpu_register(CpuRegister::A3))
    }

    fn schedule_write_special_register(
        &mut self,
        register: SpecialRegister,
        value: u32,
    ) -> Result<(), XtensaError> {
        tracing::debug!("Writing special register: {:?}", register);
        self.ensure_register_saved(CpuRegister::A3)?;
        self.state.register_cache.mark_dirty(CpuRegister::A3.into());

        self.xdm.schedule_write_ddr(value);

        // DDR -> scratch
        self.xdm
            .schedule_execute_instruction(Instruction::Rsr(SpecialRegister::Ddr, CpuRegister::A3));

        // scratch -> target special register
        self.xdm
            .schedule_execute_instruction(Instruction::Wsr(register, CpuRegister::A3));

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

    /// Write a register.
    pub fn write_register<R: TypedRegister>(&mut self, reg: R) -> Result<(), XtensaError> {
        self.write_register_untyped(R::register(), reg.as_u32())?;

        Ok(())
    }

    /// Schedules writing a register.
    pub(crate) fn schedule_write_register<R: TypedRegister>(
        &mut self,
        reg: R,
    ) -> Result<(), XtensaError> {
        self.schedule_write_register_untyped(R::register(), reg.as_u32())?;

        Ok(())
    }

    /// Schedules reading a register.
    ///
    /// If the register is already in the cache, it will return the value from there.
    pub(crate) fn schedule_read_register(
        &mut self,
        register: impl Into<Register>,
    ) -> Result<MaybeDeferredResultIndex, XtensaError> {
        let register = register.into();
        if let Some(entry) = self.state.register_cache.get_mut(register) {
            return Ok(MaybeDeferredResultIndex::Value(entry.current_value()));
        }

        let reader = match register {
            Register::Cpu(register) => self.schedule_read_cpu_register(register),
            Register::Special(register) => self.schedule_read_special_register(register)?,
            Register::CurrentPc => {
                self.schedule_read_special_register(self.core_properties.debug_level.pc())?
            }
            Register::CurrentPs => {
                self.schedule_read_special_register(self.core_properties.debug_level.ps())?
            }
        };
        Ok(MaybeDeferredResultIndex::Deferred(reader))
    }

    /// Read a register.
    pub fn read_register_untyped(
        &mut self,
        register: impl Into<Register>,
    ) -> Result<u32, XtensaError> {
        let register = register.into();
        if let Some(entry) = self.state.register_cache.get_mut(register) {
            return Ok(entry.current_value());
        }

        match self.schedule_read_register(register)? {
            MaybeDeferredResultIndex::Value(value) => Ok(value),
            MaybeDeferredResultIndex::Deferred(reader) => {
                // We need to read the register value from the target.
                let value = self.xdm.read_deferred_result(reader)?.into_u32();
                self.state.register_cache.store(register, value);
                Ok(value)
            }
        }
    }

    /// Schedules writing a register.
    ///
    /// This function primes the register cache with the value to be written, therefore
    /// it is not suitable for writing scratch registers.
    pub fn schedule_write_register_untyped(
        &mut self,
        register: impl Into<Register>,
        value: u32,
    ) -> Result<(), XtensaError> {
        let register = register.into();

        self.state.register_cache.store(register, value);

        match register {
            Register::Cpu(register) => self.schedule_write_cpu_register(register, value),
            Register::Special(register) => self.schedule_write_special_register(register, value),
            Register::CurrentPc => {
                self.schedule_write_special_register(self.core_properties.debug_level.pc(), value)
            }
            Register::CurrentPs => {
                self.schedule_write_special_register(self.core_properties.debug_level.ps(), value)
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

    /// Ensures that a scratch register is saved in the register cache before overwriting it.
    #[tracing::instrument(skip(self, register), fields(register))]
    fn ensure_register_saved(&mut self, register: impl Into<Register>) -> Result<(), XtensaError> {
        let register = register.into();

        tracing::debug!("Saving register: {:?}", register);
        self.read_register_untyped(register)?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub(super) fn restore_registers(&mut self) -> Result<(), XtensaError> {
        tracing::debug!("Restoring registers");

        let filters = [
            // First, we restore special registers, as they may need to use scratch registers.
            |r: &Register| !r.is_cpu_register(),
            // Next, we restore CPU registers, which include scratch registers.
            |r: &Register| r.is_cpu_register(),
        ];
        for filter in filters {
            // Clone the list of saved registers so we can iterate over it, but code may still save
            // new registers. We can't take it otherwise the restore loop would unnecessarily save
            // registers.
            let dirty_regs = self
                .state
                .register_cache
                .iter_mut()
                .filter(|(r, entry)| entry.is_dirty() && filter(r))
                .map(|(r, _)| r)
                .collect::<Vec<_>>();

            // First, restore special registers
            for register in dirty_regs {
                let entry = self
                    .state
                    .register_cache
                    .get_mut(register)
                    .unwrap_or_else(|| panic!("Register {register:?} is not in the cache"));

                let value = entry.original_value();
                self.schedule_write_register_untyped(register, value)?;
            }
        }

        Ok(())
    }

    fn memory_access_for(&self, address: u64, len: usize) -> Box<dyn MemoryAccess> {
        if self
            .core_properties
            .memory_range_properties(address..address + len as u64)
            .fast_memory_access
        {
            Box::new(FastMemoryAccess::new())
        } else {
            Box::new(SlowMemoryAccess::new())
        }
    }

    fn read_memory(&mut self, address: u64, dst: &mut [u8]) -> Result<(), XtensaError> {
        tracing::debug!("Reading {} bytes from address {:08x}", dst.len(), address);
        if dst.is_empty() {
            return Ok(());
        }

        let mut memory_access = self.memory_access_for(address, dst.len());

        memory_access.halted_access(self, &mut |this, memory_access| {
            memory_access.save_scratch_registers(this)?;
            this.read_memory_impl(memory_access, address, dst)
        })
    }

    fn read_memory_impl(
        &mut self,
        memory_access: &mut dyn MemoryAccess,
        address: u64,
        mut dst: &mut [u8],
    ) -> Result<(), XtensaError> {
        let mut to_read = dst.len();

        // Let's assume we can just do 32b reads, so let's
        // do some pre-massaging on unaligned reads if needed.
        let first_read = if !address.is_multiple_of(4)
            && !self
                .core_properties
                .memory_range_properties(address..address + dst.len() as u64)
                .unaligned_load
        {
            memory_access.load_initial_address_for_read(self, address as u32 & !0x3)?;
            let offset = address as usize % 4;

            // Avoid executing another read if we only have to read a single word
            let first_read = if offset + to_read <= 4 {
                memory_access.read_one(self)?
            } else {
                memory_access.read_one_and_continue(self)?
            };

            let bytes_to_copy = (4 - offset).min(to_read);

            to_read -= bytes_to_copy;

            Some((first_read, offset, bytes_to_copy))
        } else {
            // The read is either aligned or the core supports unaligned loads.
            memory_access.load_initial_address_for_read(self, address as u32)?;
            None
        };

        let mut aligned_reads = vec![];
        if to_read > 0 {
            let words = to_read.div_ceil(4);

            for _ in 0..words - 1 {
                aligned_reads.push(memory_access.read_one_and_continue(self)?);
            }
            aligned_reads.push(memory_access.read_one(self)?);
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

        Ok(())
    }

    pub(crate) fn write_memory(&mut self, address: u64, data: &[u8]) -> Result<(), XtensaError> {
        tracing::debug!("Writing {} bytes to address {:08x}", data.len(), address);
        if data.is_empty() {
            return Ok(());
        }

        let mut memory_access = self.memory_access_for(address, data.len());

        memory_access.halted_access(self, &mut |this, memory_access| {
            memory_access.save_scratch_registers(this)?;
            this.write_memory_impl(memory_access, address, data)
        })
    }

    fn write_memory_impl(
        &mut self,
        memory_access: &mut dyn MemoryAccess,
        address: u64,
        mut buffer: &[u8],
    ) -> Result<(), XtensaError> {
        let mut addr = address as u32;

        // We store the unaligned head of the data separately. In case the core supports unaligned
        // load/store, we can just write the data directly.
        let mut address_loaded = false;
        if !addr.is_multiple_of(4)
            && !self
                .core_properties
                .memory_range_properties(address..address + buffer.len() as u64)
                .unaligned_store
        {
            // If the core does not support unaligned load/store, read-modify-write the first
            // few unaligned bytes. We are calculating `unaligned_bytes` so that we are not going
            // across a word boundary here.
            let unaligned_bytes = (4 - (addr % 4) as usize).min(buffer.len());
            let aligned_address = address & !0x3;
            let offset_in_word = address as usize % 4;

            // Read the aligned word
            let mut word = [0; 4];
            self.read_memory_impl(memory_access, aligned_address, &mut word)?;

            // Replace the written bytes.
            word[offset_in_word..][..unaligned_bytes].copy_from_slice(&buffer[..unaligned_bytes]);

            // Write the word back.
            memory_access.load_initial_address_for_write(self, aligned_address as u32)?;
            memory_access.write_one(self, u32::from_le_bytes(word))?;

            buffer = &buffer[unaligned_bytes..];
            addr += unaligned_bytes as u32;

            address_loaded = true;
        }

        // Store whole words. If the core needs aligned accesses, the above block will have
        // already stored the first unaligned part.
        if buffer.len() >= 4 {
            if !address_loaded {
                memory_access.load_initial_address_for_write(self, addr)?;
            }
            let mut chunks = buffer.chunks_exact(4);
            for chunk in chunks.by_ref() {
                let mut word = [0; 4];
                word[..].copy_from_slice(chunk);
                let word = u32::from_le_bytes(word);

                memory_access.write_one(self, word)?;

                addr += 4;
            }

            buffer = chunks.remainder();
        }

        // We store the narrow tail of the data (1-3 bytes) separately.
        if !buffer.is_empty() {
            // We have 1-3 bytes left to write. If the core does not support unaligned load/store,
            // the above blocks took care of aligning `addr` so we don't have to worry about
            // crossing a word boundary here.

            // Read the aligned word
            let mut word = [0; 4];
            self.read_memory_impl(memory_access, addr as u64, &mut word)?;

            // Replace the written bytes.
            word[..buffer.len()].copy_from_slice(buffer);

            // Write the word back. We need to set the address because the read may have changed it.
            memory_access.load_initial_address_for_write(self, addr)?;
            memory_access.write_one(self, u32::from_le_bytes(word))?;
        }

        // TODO: implement cache flushing on CPUs that need it.

        Ok(())
    }

    pub(crate) fn reset_and_halt(&mut self, timeout: Duration) -> Result<(), XtensaError> {
        self.clear_register_cache();
        self.xdm.reset_and_halt()?;
        self.wait_for_core_halted(timeout)?;

        // TODO: this is only necessary to run code, so this might not be the best place
        // Make sure the CPU is in a known state and is able to run code we download.
        self.write_register({
            let mut ps = ProgramStatus(0);
            ps.set_intlevel(0);
            ps.set_user_mode(true);
            ps.set_woe(true);
            ps
        })?;
        self.restore_registers()?;

        Ok(())
    }

    pub(crate) fn clear_register_cache(&mut self) {
        self.state.register_cache = RegisterCache::new();
    }

    pub(crate) fn read_deferred_result(
        &mut self,
        result: MaybeDeferredResultIndex,
    ) -> Result<u32, XtensaError> {
        match result {
            MaybeDeferredResultIndex::Value(value) => Ok(value),
            MaybeDeferredResultIndex::Deferred(deferred_result_index) => self
                .xdm
                .read_deferred_result(deferred_result_index)
                .map(|r| r.into_u32()),
        }
    }
}

impl MemoryInterface for XtensaCommunicationInterface<'_> {
    fn read(&mut self, address: u64, dst: &mut [u8]) -> Result<(), crate::Error> {
        self.read_memory(address, dst)?;

        Ok(())
    }

    fn supports_native_64bit_access(&mut self) -> bool {
        false
    }

    fn read_word_64(&mut self, address: u64) -> Result<u64, crate::Error> {
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

    fn read_word_8(&mut self, address: u64) -> Result<u8, crate::Error> {
        let mut out = 0;
        self.read(address, std::slice::from_mut(&mut out))?;
        Ok(out)
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), crate::Error> {
        self.read_8(address, data.as_mut_bytes())
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), crate::Error> {
        self.read_8(address, data.as_mut_bytes())
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), crate::Error> {
        self.read_8(address, data.as_mut_bytes())
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), crate::Error> {
        self.read(address, data)
    }

    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), crate::Error> {
        self.write_memory(address, data)?;

        Ok(())
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), crate::Error> {
        self.write(address, &data.to_le_bytes())
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), crate::Error> {
        self.write(address, &data.to_le_bytes())
    }

    fn write_word_16(&mut self, address: u64, data: u16) -> Result<(), crate::Error> {
        self.write(address, &data.to_le_bytes())
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), crate::Error> {
        self.write(address, &[data])
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), crate::Error> {
        self.write_8(address, data.as_bytes())
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), crate::Error> {
        self.write_8(address, data.as_bytes())
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), crate::Error> {
        self.write_8(address, data.as_bytes())
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), crate::Error> {
        self.write(address, data)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, crate::Error> {
        Ok(true)
    }

    fn flush(&mut self) -> Result<(), crate::Error> {
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

/// The Program Counter register.
#[derive(Copy, Clone, Debug)]
pub struct ProgramCounter(pub u32);
u32_register!(ProgramCounter, Register::CurrentPc);

trait MemoryAccess {
    fn halted_access(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
        op: &mut dyn FnMut(
            &mut XtensaCommunicationInterface,
            &mut dyn MemoryAccess,
        ) -> Result<(), XtensaError>,
    ) -> Result<(), XtensaError>;

    fn save_scratch_registers(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
    ) -> Result<(), XtensaError>;

    fn load_initial_address_for_read(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
        address: u32,
    ) -> Result<(), XtensaError>;
    fn load_initial_address_for_write(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
        address: u32,
    ) -> Result<(), XtensaError>;

    fn read_one(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
    ) -> Result<DeferredResultIndex, XtensaError>;

    fn read_one_and_continue(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
    ) -> Result<DeferredResultIndex, XtensaError>;

    fn write_one(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
        data: u32,
    ) -> Result<(), XtensaError>;
}

/// Memory access using LDDR32.P and SDDR32.P instructions.
struct FastMemoryAccess;
impl FastMemoryAccess {
    fn new() -> Self {
        Self
    }
}
impl MemoryAccess for FastMemoryAccess {
    fn halted_access(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
        op: &mut dyn FnMut(
            &mut XtensaCommunicationInterface,
            &mut dyn MemoryAccess,
        ) -> Result<(), XtensaError>,
    ) -> Result<(), XtensaError> {
        interface.fast_halted_access(|this| op(this, self))
    }

    fn save_scratch_registers(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
    ) -> Result<(), XtensaError> {
        interface.ensure_register_saved(CpuRegister::A3)?;
        Ok(())
    }

    fn load_initial_address_for_read(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
        address: u32,
    ) -> Result<(), XtensaError> {
        // Write aligned address to the scratch register
        interface.schedule_write_cpu_register(CpuRegister::A3, address)?;
        interface
            .state
            .register_cache
            .mark_dirty(CpuRegister::A3.into());

        // Read from address in the scratch register
        interface
            .xdm
            .schedule_execute_instruction(Instruction::Lddr32P(CpuRegister::A3));

        Ok(())
    }

    fn load_initial_address_for_write(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
        address: u32,
    ) -> Result<(), XtensaError> {
        interface.schedule_write_cpu_register(CpuRegister::A3, address)?;
        interface
            .state
            .register_cache
            .mark_dirty(CpuRegister::A3.into());

        interface
            .xdm
            .schedule_write_instruction(Instruction::Sddr32P(CpuRegister::A3));

        Ok(())
    }

    fn write_one(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
        data: u32,
    ) -> Result<(), XtensaError> {
        interface.xdm.schedule_write_ddr_and_execute(data);
        Ok(())
    }

    fn read_one(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
    ) -> Result<DeferredResultIndex, XtensaError> {
        Ok(interface.xdm.schedule_read_ddr())
    }

    fn read_one_and_continue(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
    ) -> Result<DeferredResultIndex, XtensaError> {
        Ok(interface.xdm.schedule_read_ddr_and_execute())
    }
}

/// Memory access without LDDR32.P and SDDR32.P instructions.
struct SlowMemoryAccess {
    current_address: u32,
    current_offset: u32,
    address_written: bool,
}
impl SlowMemoryAccess {
    fn new() -> Self {
        Self {
            current_address: 0,
            current_offset: 0,
            address_written: false,
        }
    }
}

impl MemoryAccess for SlowMemoryAccess {
    fn halted_access(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
        op: &mut dyn FnMut(
            &mut XtensaCommunicationInterface,
            &mut dyn MemoryAccess,
        ) -> Result<(), XtensaError>,
    ) -> Result<(), XtensaError> {
        interface.halted_access(|this| op(this, self))
    }

    fn save_scratch_registers(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
    ) -> Result<(), XtensaError> {
        interface.ensure_register_saved(CpuRegister::A3)?;
        interface.ensure_register_saved(CpuRegister::A4)?;
        Ok(())
    }

    fn load_initial_address_for_read(
        &mut self,
        _interface: &mut XtensaCommunicationInterface,
        address: u32,
    ) -> Result<(), XtensaError> {
        self.current_address = address;

        Ok(())
    }

    fn load_initial_address_for_write(
        &mut self,
        _interface: &mut XtensaCommunicationInterface,
        address: u32,
    ) -> Result<(), XtensaError> {
        self.current_address = address;

        Ok(())
    }

    fn read_one(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
    ) -> Result<DeferredResultIndex, XtensaError> {
        if !self.address_written {
            interface.schedule_write_cpu_register(CpuRegister::A3, self.current_address)?;
            interface
                .state
                .register_cache
                .mark_dirty(CpuRegister::A3.into());
            self.current_offset = 0;
            self.address_written = true;
        }

        interface
            .xdm
            .schedule_execute_instruction(Instruction::L32I(
                CpuRegister::A3,
                CpuRegister::A4,
                (self.current_offset / 4) as u8,
            ));
        self.current_offset += 4;

        interface
            .state
            .register_cache
            .mark_dirty(CpuRegister::A4.into());

        if self.current_offset == 1024 {
            // The maximum offset for L32I is 1020, so we need to
            // increment the base address and reset the offset.
            self.current_address += self.current_offset;
            self.current_offset = 0;
            self.address_written = false;
        }

        Ok(interface.schedule_read_cpu_register(CpuRegister::A4))
    }

    fn read_one_and_continue(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
    ) -> Result<DeferredResultIndex, XtensaError> {
        self.read_one(interface)
    }

    fn write_one(
        &mut self,
        interface: &mut XtensaCommunicationInterface,
        data: u32,
    ) -> Result<(), XtensaError> {
        // Store address and data
        interface.schedule_write_cpu_register(CpuRegister::A3, self.current_address)?;
        interface
            .state
            .register_cache
            .mark_dirty(CpuRegister::A3.into());

        interface.schedule_write_cpu_register(CpuRegister::A4, data)?;
        interface
            .state
            .register_cache
            .mark_dirty(CpuRegister::A4.into());

        // Increment address
        self.current_address += 4;

        // Store A4 into address A3
        interface
            .xdm
            .schedule_execute_instruction(Instruction::S32I(CpuRegister::A3, CpuRegister::A4, 0));

        Ok(())
    }
}

pub(crate) enum MaybeDeferredResultIndex {
    /// The result is already available.
    Value(u32),

    /// The result is deferred.
    Deferred(DeferredResultIndex),
}
