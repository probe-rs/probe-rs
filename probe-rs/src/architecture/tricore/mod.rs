//! Infineon AURIX TriCore support.
//!
//! TriCore debug (OCDS) is entirely memory mapped. Every core has a CSFR window that holds its
//! debug status register (`DBGSR`), its trigger units (`TR0`-`TR7`) and its core registers.
//! A probe only provides memory access and a device reset ([`bus::TriCoreBusAccess`]); the core
//! control below is the same for every probe.
//!
//! The run and single step sequences are the ones of the Infineon reference debugger for the
//! DAS access hardware, verified on a TC275 (C-step).

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use probe_rs_target::{Architecture, CoreType, InstructionSet, TriCoreCoreAccessOptions};

use crate::{
    CoreInformation, CoreInterface, CoreRegister, CoreStatus, Error, HaltReason,
    MemoryInterface,
    core::{
        BreakpointCause,
        registers::{CoreRegisters, RegisterId, RegisterValue},
    },
    memory::{CoreMemoryInterface, InvalidDataLengthError, MemoryNotAlignedError},
};

pub mod bus;
pub mod registers;
pub mod sequences;

use bus::{AddressMap, ResetKind, TriCoreBusAccess, TriCoreError};
use registers::{FP, PC, RA, SP, TRICORE_CORE_REGISTERS};
use sequences::TriCoreDebugSequence;

/// Offsets in the CSFR window of a core, and fixed OCDS addresses.
pub mod ocds {
    /// Debug status register.
    pub const DBGSR: u16 = 0xFD00;
    /// Debug configuration register, written by the debug init.
    pub const DBG_CFG: u16 = 0xFD10;
    /// Trigger accumulator: one bit per trigger unit that fired. Reading clears it.
    pub const TRIG_ACC: u16 = 0xFD30;
    /// `TR0EVT`. `TRnEVT` is at `TRIG + 8 * n`, `TRnADR` at `TRIG + 4 + 8 * n`.
    pub const TRIG: u16 = 0xF000;
    /// The number of trigger units of a core.
    pub const TRIGGER_UNITS: usize = 8;

    /// `DBGSR` write: clear `HALT` (run).
    pub const DBGSR_RUN: u32 = 0x4;
    /// `DBGSR` write: set `HALT` (halt).
    pub const DBGSR_HALT: u32 = 0x6;

    /// `TRnEVT` value of an instruction (PC) breakpoint: break before make, halt.
    pub const TRIG_EVT_IP: u32 = 0x0000_101A;

    /// Control register of the DAS access hardware (in the CBS address range).
    pub const ACCESS_HW_CTRL: u32 = 0xF000_0494;
    /// Run command register of the DAS access hardware.
    pub const ACCESS_HW_RUNCMD: u32 = 0xF000_049C;
    /// `ACCESS_HW_CTRL` value that brackets a `DBGSR` run write (start).
    pub const ACCESS_HW_STEP_ON: u32 = 0x8000_003E;
    /// `ACCESS_HW_CTRL` value that brackets a `DBGSR` run write (end).
    pub const ACCESS_HW_STEP_OFF: u32 = 0x8000_003F;

    /// The `HALT` field (bits 2:1) of a `DBGSR` value.
    pub fn halt_field(dbgsr: u32) -> u32 {
        (dbgsr >> 1) & 0b11
    }

    /// Whether a `DBGSR` value reports a halted core (`HALT` = 01b).
    pub fn is_halted(dbgsr: u32) -> bool {
        halt_field(dbgsr) == 0b01
    }

    /// Whether a `DBGSR` value reports a running core (`HALT` = 00b).
    pub fn is_running(dbgsr: u32) -> bool {
        halt_field(dbgsr) == 0b00
    }

    /// The trigger window values of the debug init, before a single step.
    pub const STEP_TRIGGERS_INIT: [u32; 16] = [
        0x2000, 0xFFFF_FFFF, 0x2000, 0xFFFF_FFFF, 0x2000, 0xFFFF_FFFF, 0x2000, 0xFFFF_FFFF, 0x2000,
        0xFFFF_FFFF, 0x2000, 0xFFFF_FFFF, 0x2000, 0xFFFF_FFFF, 0x2000, 0xFFFF_FFFF,
    ];
    /// The trigger window values that arm the single step trap (`TR0`).
    pub const STEP_TRIGGERS_ARM: [u32; 16] = [
        0x3002, 0, 0, 0xFFFF_FFFF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    /// The value written to [`DBG_CFG`] by the debug init.
    pub const DBG_CFG_VALUE: u32 = 0x2A;
}

/// The local scratchpad windows of a core, and their global aliases.
mod spr {
    pub const DSPR_LOCAL: u32 = 0xD000_0000;
    pub const DSPR_SIZE: u32 = 0x2_0000;
    pub const DSPR_GLOBAL: u32 = 0x7000_0000;
    pub const PSPR_LOCAL: u32 = 0xC000_0000;
    pub const PSPR_SIZE: u32 = 0x8000;
    pub const PSPR_GLOBAL: u32 = 0x7010_0000;
    pub const CORE_STEP: u32 = 0x1000_0000;
}

/// Translates a core local scratchpad address (`0xC...`/`0xD...`) to the global alias that the
/// debug interface sees. Other addresses are returned unchanged.
pub fn translate_local_address(core_index: u8, address: u32) -> u32 {
    let base = core_index as u32 * spr::CORE_STEP;
    if (spr::DSPR_LOCAL..spr::DSPR_LOCAL + spr::DSPR_SIZE).contains(&address) {
        spr::DSPR_GLOBAL - base + (address - spr::DSPR_LOCAL)
    } else if (spr::PSPR_LOCAL..spr::PSPR_LOCAL + spr::PSPR_SIZE).contains(&address) {
        spr::PSPR_GLOBAL - base + (address - spr::PSPR_LOCAL)
    } else {
        address
    }
}

/// What the debugger last asked the core to do. Used to tell why the core halted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastAction {
    None,
    Halt,
    Run,
    Step,
}

/// The state of a TriCore core that persists between [`TriCore`] handles.
#[derive(Debug)]
pub struct TriCoreCoreState {
    /// The planted hardware breakpoints, per trigger unit.
    breakpoints: [Option<u32>; ocds::TRIGGER_UNITS],
    /// Whether hardware breakpoints are enabled.
    breakpoints_enabled: bool,
    /// Whether the core was halted at the last status check.
    was_halted: bool,
    /// Why the core halted, once known.
    halt_reason: Option<HaltReason>,
    /// Trigger accumulator bits read before the halt was reported.
    pending_trigger_acc: u32,
    last_action: LastAction,
}

impl TriCoreCoreState {
    /// Creates the state of a core that was not attached yet.
    pub(crate) fn new() -> Self {
        Self {
            breakpoints: [None; ocds::TRIGGER_UNITS],
            breakpoints_enabled: false,
            was_halted: false,
            halt_reason: None,
            pending_trigger_acc: 0,
            last_action: LastAction::None,
        }
    }
}

/// Memory access of one TriCore core.
///
/// Local scratchpad addresses are translated to the global aliases of the core.
#[derive(Debug)]
pub struct TriCoreMemory<'probe> {
    bus: &'probe mut dyn TriCoreBusAccess,
    core_index: u8,
}

impl<'probe> TriCoreMemory<'probe> {
    /// Creates the memory interface of core `core_index`.
    pub fn new(bus: &'probe mut dyn TriCoreBusAccess, core_index: u8) -> Self {
        Self { bus, core_index }
    }

    fn address(&self, address: u64) -> Result<u32, TriCoreError> {
        let address = u32::try_from(address).map_err(|_| {
            TriCoreError::Protocol(format!("address {address:#x} is outside the 32-bit space"))
        })?;
        Ok(translate_local_address(self.core_index, address))
    }

    fn read_bytes(&mut self, address: u64, data: &mut [u8]) -> Result<(), TriCoreError> {
        let address = self.address(address)?;
        self.bus.read_block(AddressMap::Target, address, data)
    }

    fn write_bytes(&mut self, address: u64, data: &[u8]) -> Result<(), TriCoreError> {
        let address = self.address(address)?;
        self.bus.write_block(AddressMap::Target, address, data)
    }
}

fn check_alignment(address: u64, alignment: usize) -> Result<(), TriCoreError> {
    if address.is_multiple_of(alignment as u64) {
        Ok(())
    } else {
        Err(MemoryNotAlignedError { address, alignment }.into())
    }
}

impl MemoryInterface<TriCoreError> for TriCoreMemory<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        // Byte reads use block transfers, so `read` goes straight to `read_8`.
        true
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), TriCoreError> {
        check_alignment(address, 8)?;
        let mut bytes = vec![0; data.len() * 8];
        self.read_bytes(address, &mut bytes)?;
        for (word, chunk) in data.iter_mut().zip(bytes.chunks_exact(8)) {
            *word = u64::from_le_bytes(chunk.try_into().unwrap_or_default());
        }
        Ok(())
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), TriCoreError> {
        check_alignment(address, 4)?;
        let mut bytes = vec![0; data.len() * 4];
        self.read_bytes(address, &mut bytes)?;
        for (word, chunk) in data.iter_mut().zip(bytes.chunks_exact(4)) {
            *word = u32::from_le_bytes(chunk.try_into().unwrap_or_default());
        }
        Ok(())
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), TriCoreError> {
        check_alignment(address, 2)?;
        let mut bytes = vec![0; data.len() * 2];
        self.read_bytes(address, &mut bytes)?;
        for (word, chunk) in data.iter_mut().zip(bytes.chunks_exact(2)) {
            *word = u16::from_le_bytes(chunk.try_into().unwrap_or_default());
        }
        Ok(())
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), TriCoreError> {
        self.read_bytes(address, data)
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), TriCoreError> {
        check_alignment(address, 8)?;
        let bytes = data.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<_>>();
        self.write_bytes(address, &bytes)
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), TriCoreError> {
        check_alignment(address, 4)?;
        let bytes = data.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<_>>();
        self.write_bytes(address, &bytes)
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), TriCoreError> {
        check_alignment(address, 2)?;
        let bytes = data.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<_>>();
        self.write_bytes(address, &bytes)
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), TriCoreError> {
        self.write_bytes(address, data)
    }

    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), TriCoreError> {
        self.write_bytes(address, data)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, TriCoreError> {
        Ok(true)
    }

    fn flush(&mut self) -> Result<(), TriCoreError> {
        Ok(())
    }
}

impl From<InvalidDataLengthError> for TriCoreError {
    fn from(error: InvalidDataLengthError) -> Self {
        TriCoreError::Protocol(error.to_string())
    }
}

impl From<MemoryNotAlignedError> for TriCoreError {
    fn from(error: MemoryNotAlignedError) -> Self {
        TriCoreError::Protocol(error.to_string())
    }
}

/// An interface to operate a TriCore core.
pub struct TriCore<'probe> {
    memory: TriCoreMemory<'probe>,
    state: &'probe mut TriCoreCoreState,
    options: TriCoreCoreAccessOptions,
    #[expect(dead_code, reason = "kept for chip specific core hooks")]
    sequence: Arc<dyn TriCoreDebugSequence>,
}

impl<'probe> TriCore<'probe> {
    /// Creates the interface of one TriCore core.
    pub fn new(
        bus: &'probe mut dyn TriCoreBusAccess,
        state: &'probe mut TriCoreCoreState,
        options: TriCoreCoreAccessOptions,
        sequence: Arc<dyn TriCoreDebugSequence>,
    ) -> Self {
        Self {
            memory: TriCoreMemory::new(bus, options.core_index),
            state,
            options,
            sequence,
        }
    }

    fn core(&self) -> u8 {
        self.options.core_index
    }

    fn csfr(&self, offset: u16) -> u32 {
        self.options.csfr_base as u32 + offset as u32
    }

    fn bus(&mut self) -> &mut dyn TriCoreBusAccess {
        &mut *self.memory.bus
    }

    fn read_csfr(&mut self, offset: u16) -> Result<u32, TriCoreError> {
        let address = self.csfr(offset);
        self.bus().read_u32(AddressMap::Target, address)
    }

    fn write_csfr(&mut self, offset: u16, value: u32) -> Result<(), TriCoreError> {
        let address = self.csfr(offset);
        self.bus().write_u32(AddressMap::Target, address, value)
    }

    fn write_csfr_words(&mut self, offset: u16, values: &[u32]) -> Result<(), TriCoreError> {
        let address = self.csfr(offset);
        let bytes = values.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<_>>();
        self.bus().write_block(AddressMap::Target, address, &bytes)
    }

    fn write_access_hw(&mut self, address: u32, value: u32) -> Result<(), TriCoreError> {
        self.bus().write_u32(AddressMap::Target, address, value)
    }

    fn dbgsr(&mut self) -> Result<u32, TriCoreError> {
        self.read_csfr(ocds::DBGSR)
    }

    fn pc(&mut self) -> Result<u32, TriCoreError> {
        self.read_csfr(registers::CSFR_PC)
    }

    fn core_info(&mut self) -> Result<CoreInformation, Error> {
        Ok(CoreInformation {
            pc: self.pc()? as u64,
        })
    }

    fn trigger_evt(unit: usize) -> u16 {
        ocds::TRIG + 8 * unit as u16
    }

    fn trigger_adr(unit: usize) -> u16 {
        ocds::TRIG + 4 + 8 * unit as u16
    }

    /// Writes the trigger unit of one breakpoint from the state: armed when it is set and
    /// breakpoints are enabled, disabled otherwise.
    fn write_trigger(&mut self, unit: usize) -> Result<(), TriCoreError> {
        match self.state.breakpoints[unit] {
            Some(address) if self.state.breakpoints_enabled => {
                self.write_csfr(Self::trigger_adr(unit), address)?;
                self.write_csfr(Self::trigger_evt(unit), ocds::TRIG_EVT_IP)
            }
            _ => self.write_csfr(Self::trigger_evt(unit), 0),
        }
    }

    /// Rewrites the trigger units that hold a breakpoint. The single step sequence reprograms
    /// the trigger window, so the breakpoints are restored after it.
    fn rearm_triggers(&mut self) -> Result<(), TriCoreError> {
        for unit in 0..ocds::TRIGGER_UNITS {
            if self.state.breakpoints[unit].is_some() {
                self.write_trigger(unit)?;
            }
        }
        Ok(())
    }

    /// Rewrites every trigger unit from the state and discards the trigger accumulator. After a
    /// device reset the boot software leaves its own trigger (halt after reset) behind.
    fn reinit_triggers(&mut self) -> Result<(), TriCoreError> {
        for unit in 0..ocds::TRIGGER_UNITS {
            self.write_trigger(unit)?;
        }
        self.read_csfr(ocds::TRIG_ACC)?;
        self.state.pending_trigger_acc = 0;
        Ok(())
    }

    /// The reference debug init: reprogram the trigger window, clear `DBGSR`..`DBGSR+0xF` and
    /// write the debug configuration. `arm_step` also arms the single step trap.
    fn step_debug_init(&mut self, arm_step: bool) -> Result<(), TriCoreError> {
        self.write_csfr_words(ocds::TRIG, &ocds::STEP_TRIGGERS_INIT)?;
        self.write_csfr_words(ocds::DBGSR, &[0; 4])?;
        self.write_csfr(ocds::DBG_CFG, ocds::DBG_CFG_VALUE)?;
        if arm_step {
            self.write_csfr_words(ocds::TRIG, &ocds::STEP_TRIGGERS_ARM)?;
        }
        Ok(())
    }

    /// Called once the core is seen halted after it ran: works out why it halted.
    fn on_halted(&mut self) -> Result<(), TriCoreError> {
        let trigger_acc = self.read_csfr(ocds::TRIG_ACC)? | self.state.pending_trigger_acc;
        self.state.pending_trigger_acc = 0;
        let pc = self.pc()?;

        // A trigger accumulator bit alone does not prove that a breakpoint fired: the boot
        // software and the step trap use TR0 too. An instruction breakpoint halts with the PC at
        // its address (break before make), so require that.
        let breakpoint_hit = self.state.breakpoints_enabled
            && self.state.breakpoints.iter().enumerate().any(|(unit, bp)| {
                trigger_acc & (1 << unit) != 0 && *bp == Some(pc)
            });

        let reason = if breakpoint_hit {
            HaltReason::Breakpoint(BreakpointCause::Hardware)
        } else {
            match self.state.last_action {
                LastAction::Step => HaltReason::Step,
                LastAction::Halt => HaltReason::Request,
                LastAction::Run if trigger_acc != 0 => {
                    HaltReason::Breakpoint(BreakpointCause::Unknown)
                }
                _ => HaltReason::Unknown,
            }
        };
        tracing::debug!(
            "TriCore core {} halted at {pc:#010x}: {reason:?} (TRIG_ACC {trigger_acc:#x})",
            self.core()
        );
        self.state.halt_reason = Some(reason);
        Ok(())
    }

    fn update_halted(&mut self) -> Result<bool, TriCoreError> {
        let halted = ocds::is_halted(self.dbgsr()?);
        if halted && !self.state.was_halted {
            self.on_halted()?;
        }
        if !halted {
            self.state.halt_reason = None;
        }
        self.state.was_halted = halted;
        Ok(halted)
    }

    fn halt_core(&mut self, timeout: Duration) -> Result<(), TriCoreError> {
        if self.update_halted()? {
            return Ok(());
        }
        self.state.last_action = LastAction::Halt;
        self.write_csfr(ocds::DBGSR, ocds::DBGSR_HALT)?;

        let start = Instant::now();
        loop {
            if self.update_halted()? {
                return Ok(());
            }
            if start.elapsed() > timeout {
                let dbgsr = self.dbgsr()?;
                return Err(TriCoreError::CoreDidNotHalt {
                    core: self.core(),
                    dbgsr,
                });
            }
        }
    }

    /// Whether the core is halted at a planted instruction breakpoint. Running from there would
    /// hit the same trigger again, so the instruction is stepped first.
    fn at_breakpoint(&mut self) -> Result<bool, TriCoreError> {
        if !self.state.breakpoints_enabled || self.state.breakpoints.iter().all(Option::is_none) {
            return Ok(false);
        }
        let pc = self.pc()?;
        Ok(self.state.breakpoints.contains(&Some(pc)))
    }

    fn step_core(&mut self) -> Result<(), TriCoreError> {
        let dbgsr = self.dbgsr()?;
        if !ocds::is_halted(dbgsr) {
            return Err(TriCoreError::NotHalted(self.core()));
        }

        self.step_debug_init(true)?;
        self.write_access_hw(ocds::ACCESS_HW_CTRL, ocds::ACCESS_HW_STEP_ON)?;
        self.write_csfr(ocds::DBGSR, ocds::DBGSR_RUN)?;
        self.write_access_hw(ocds::ACCESS_HW_CTRL, ocds::ACCESS_HW_STEP_OFF)?;
        self.step_debug_init(false)?;
        // The debug init disabled the trigger units, restore the breakpoints.
        self.rearm_triggers()?;

        for _ in 0..20 {
            if ocds::is_halted(self.dbgsr()?) {
                // The halt came from the step trap in TR0. Its accumulator bit is not a
                // breakpoint hit: discard it (reading clears it).
                self.read_csfr(ocds::TRIG_ACC)?;
                self.state.pending_trigger_acc = 0;
                self.state.last_action = LastAction::Step;
                self.state.halt_reason = Some(HaltReason::Step);
                self.state.was_halted = true;
                return Ok(());
            }
        }
        Err(TriCoreError::StepFailed(self.core()))
    }

    fn run_core(&mut self) -> Result<(), TriCoreError> {
        if !ocds::is_halted(self.dbgsr()?) {
            // Already running.
            return Ok(());
        }
        if self.at_breakpoint()? {
            self.step_core()?;
        }

        self.state.last_action = LastAction::Run;
        self.write_access_hw(ocds::ACCESS_HW_CTRL, ocds::ACCESS_HW_STEP_ON)?;
        self.write_csfr(ocds::DBGSR, ocds::DBGSR_RUN)?;
        self.write_access_hw(ocds::ACCESS_HW_CTRL, ocds::ACCESS_HW_STEP_OFF)?;
        self.write_csfr(ocds::DBGSR, ocds::DBGSR_RUN)?;
        self.write_access_hw(ocds::ACCESS_HW_RUNCMD, 2)?;

        let dbgsr = self.dbgsr()?;
        if !ocds::is_running(dbgsr) {
            // A trigger that catches the core right after the start is a normal halt: keep its
            // accumulator bits for the halt reason.
            let trigger_acc = self.read_csfr(ocds::TRIG_ACC)?;
            if trigger_acc == 0 {
                return Err(TriCoreError::CoreDidNotRun {
                    core: self.core(),
                    dbgsr,
                });
            }
            self.state.pending_trigger_acc |= trigger_acc;
        }
        self.state.was_halted = false;
        self.state.halt_reason = None;
        Ok(())
    }

    fn reset_and_halt_core(&mut self, timeout: Duration) -> Result<(), TriCoreError> {
        self.bus().reset(ResetKind::ResetAndHalt)?;

        // The device reboots: CSFR accesses fail for a short while.
        let start = Instant::now();
        loop {
            match self.dbgsr() {
                Ok(_) => break,
                Err(error) if start.elapsed() > timeout => return Err(error),
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        self.reinit_triggers()?;
        self.state.last_action = LastAction::Halt;
        self.state.was_halted = false;
        self.halt_core(timeout)?;
        self.state.halt_reason = Some(HaltReason::Request);
        Ok(())
    }

    fn ensure_unit(&self, unit_index: usize) -> Result<(), TriCoreError> {
        if unit_index >= ocds::TRIGGER_UNITS {
            return Err(TriCoreError::BreakpointOutOfBounds(unit_index));
        }
        Ok(())
    }
}

impl<'probe> CoreMemoryInterface for TriCore<'probe> {
    type ErrorType = TriCoreError;

    fn memory(&self) -> &dyn MemoryInterface<Self::ErrorType> {
        &self.memory
    }

    fn memory_mut(&mut self) -> &mut dyn MemoryInterface<Self::ErrorType> {
        &mut self.memory
    }
}

impl CoreInterface for TriCore<'_> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        let start = Instant::now();
        while !self.update_halted()? {
            if start.elapsed() > timeout {
                return Err(Error::Timeout);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }

    fn core_halted(&mut self) -> Result<bool, Error> {
        Ok(self.update_halted()?)
    }

    fn status(&mut self) -> Result<CoreStatus, Error> {
        if self.update_halted()? {
            Ok(CoreStatus::Halted(
                self.state.halt_reason.unwrap_or(HaltReason::Unknown),
            ))
        } else {
            Ok(CoreStatus::Running)
        }
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.halt_core(timeout)?;
        self.core_info()
    }

    fn run(&mut self) -> Result<(), Error> {
        Ok(self.run_core()?)
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.reset_and_halt_core(Duration::from_millis(1000))?;
        Ok(self.run_core()?)
    }

    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.reset_and_halt_core(timeout)?;
        self.core_info()
    }

    fn step(&mut self) -> Result<CoreInformation, Error> {
        self.step_core()?;
        self.core_info()
    }

    fn read_core_reg(&mut self, address: RegisterId) -> Result<RegisterValue, Error> {
        Ok(RegisterValue::U32(self.read_csfr(address.0)?))
    }

    fn write_core_reg(&mut self, address: RegisterId, value: RegisterValue) -> Result<(), Error> {
        if !ocds::is_halted(self.dbgsr()?) {
            return Err(TriCoreError::NotHalted(self.core()).into());
        }
        let value: u32 = value.try_into()?;
        Ok(self.write_csfr(address.0, value)?)
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        Ok(ocds::TRIGGER_UNITS as u32)
    }

    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        Ok(self
            .state
            .breakpoints
            .iter()
            .map(|bp| bp.map(u64::from))
            .collect())
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), Error> {
        self.state.breakpoints_enabled = state;
        for unit in 0..ocds::TRIGGER_UNITS {
            if self.state.breakpoints[unit].is_some() {
                self.write_trigger(unit)?;
            }
        }
        Ok(())
    }

    fn set_hw_breakpoint(&mut self, unit_index: usize, addr: u64) -> Result<(), Error> {
        self.ensure_unit(unit_index)?;
        let address = u32::try_from(addr)
            .map_err(|_| Error::Other(format!("breakpoint address {addr:#x} out of range")))?;
        self.state.breakpoints[unit_index] = Some(address);
        Ok(self.write_trigger(unit_index)?)
    }

    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), Error> {
        self.ensure_unit(unit_index)?;
        self.state.breakpoints[unit_index] = None;
        Ok(self.write_trigger(unit_index)?)
    }

    fn registers(&self) -> &'static CoreRegisters {
        &TRICORE_CORE_REGISTERS
    }

    fn program_counter(&self) -> &'static CoreRegister {
        &PC
    }

    fn frame_pointer(&self) -> &'static CoreRegister {
        &FP
    }

    fn stack_pointer(&self) -> &'static CoreRegister {
        &SP
    }

    fn return_address(&self) -> &'static CoreRegister {
        &RA
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        self.state.breakpoints_enabled
    }

    fn architecture(&self) -> Architecture {
        Architecture::TriCore
    }

    fn core_type(&self) -> CoreType {
        CoreType::TriCore
    }

    fn instruction_set(&mut self) -> Result<InstructionSet, Error> {
        Ok(InstructionSet::TriCore)
    }

    fn fpu_support(&mut self) -> Result<bool, Error> {
        // The TC1.6 FPU works on the data registers; there is no separate register file.
        Ok(false)
    }

    fn floating_point_register_count(&mut self) -> Result<usize, Error> {
        Ok(0)
    }

    fn reset_catch_set(&mut self) -> Result<(), Error> {
        Err(Error::NotImplemented("reset_catch_set"))
    }

    fn reset_catch_clear(&mut self) -> Result<(), Error> {
        Err(Error::NotImplemented("reset_catch_clear"))
    }

    fn debug_core_stop(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
