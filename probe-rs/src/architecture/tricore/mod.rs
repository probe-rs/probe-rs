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
    CoreInformation, CoreInterface, CoreRegister, CoreStatus, Error, HaltReason, MemoryInterface,
    core::{
        BreakpointCause,
        registers::{CoreRegisters, RegisterId, RegisterValue},
    },
    memory::{CoreMemoryInterface, InvalidDataLengthError, MemoryNotAlignedError},
};

pub mod bus;
pub mod registers;
pub mod sequences;

use bus::{AddressMap, BusTransfer, ResetKind, TriCoreBusAccess, TriCoreError};
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

    /// Whether a `TRnEVT` value halts the core when the trigger fires (`EVTA` = 010b).
    pub fn is_halt_trigger(evt: u32) -> bool {
        evt & 0b111 == 0b010
    }

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
        0x2000,
        0xFFFF_FFFF,
        0x2000,
        0xFFFF_FFFF,
        0x2000,
        0xFFFF_FFFF,
        0x2000,
        0xFFFF_FFFF,
        0x2000,
        0xFFFF_FFFF,
        0x2000,
        0xFFFF_FFFF,
        0x2000,
        0xFFFF_FFFF,
        0x2000,
        0xFFFF_FFFF,
    ];
    /// The trigger window values that arm the single step trap (`TR0`).
    pub const STEP_TRIGGERS_ARM: [u32; 16] = [
        0x3002,
        0,
        0,
        0xFFFF_FFFF,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    /// The value written to [`DBG_CFG`] by the debug init.
    pub const DBG_CFG_VALUE: u32 = 0x2A;

    /// External event register: the debug action on the core break-in signal.
    pub const EXEVT: u16 = 0xFD08;
    /// `EXEVT` value of a halt group member: halt on break-in, without a break-out pulse.
    pub const EXEVT_HALT: u32 = 0x12;

    /// The `EVTSRC` field (bits 12:8) of a `DBGSR` value. It is 0 after an external event:
    /// the core break-in signal, or a halt request written to `DBGSR`.
    pub fn event_source(dbgsr: u32) -> u32 {
        (dbgsr >> 8) & 0x1F
    }

    /// `CBS_TRC0`, the trigger switch routing of CPU0. `CBS_TRCn` follows every 4 bytes.
    pub const CBS_TRC0: u32 = 0xF000_0420;
    /// The trigger switch line that connects the cores of the halt group.
    pub const HALT_GROUP_LINE: u32 = 7;
    /// The `HALT` (halt state to a line, bits 3:0) and `BRKIN` (a line to the core break-in,
    /// bits 23:20) fields of `CBS_TRCn`.
    pub const TRC_HALT_BRKIN: u32 = 0x00F0_000F;
    /// `HALT` and `BRKIN` of a halt group member: both on [`HALT_GROUP_LINE`].
    pub const TRC_HALT_GROUP: u32 = HALT_GROUP_LINE | (HALT_GROUP_LINE << 20);

    /// The address of `CBS_TRCn` for CPU `core_index`.
    pub fn trc(core_index: u8) -> u32 {
        CBS_TRC0 + 4 * core_index as u32
    }

    /// Whether a `CBS_TRCn` value puts the core in the halt group.
    pub fn in_halt_group(trc: u32) -> bool {
        trc & TRC_HALT_BRKIN == TRC_HALT_GROUP
    }
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

/// How long CSFR accesses may fail while the device reboots after a reset.
const REBOOT_TIME: Duration = Duration::from_secs(2);

/// The halt group registers of a core and its peers, read in one request.
struct GroupState {
    /// `CBS_TRCn` of the core.
    trc: u32,
    /// `EXEVT` of the core.
    exevt: u32,
    /// (core index, `CBS_TRCn`, `DBGSR`) of every peer.
    peers: Vec<(u8, u32, u32)>,
}

impl GroupState {
    /// The peers that are in the halt group and run.
    fn running_members(&self) -> Vec<u8> {
        self.peers
            .iter()
            .filter(|&&(_, trc, dbgsr)| ocds::in_halt_group(trc) && ocds::is_running(dbgsr))
            .map(|&(core, _, _)| core)
            .collect()
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
    /// Whether this session has taken over the trigger units of the core: it cleared what an
    /// earlier session left there, and keeps the units in step with `breakpoints` since.
    ///
    /// A session takes over a core only when it first steps it or changes its breakpoints, so
    /// that several sessions (one per core, for example one debugger per core) can share a
    /// device without clearing each other's breakpoints.
    owns_triggers: bool,
    /// Set while this session keeps the core in the halt group: the `CBS_TRCn` and `EXEVT`
    /// values from before, put back when the core leaves the group.
    halt_group: Option<(u32, u32)>,
    /// The other halt group members seen running at the last status check of this running core.
    /// Only these run again with this core: a core that never ran since the reset (CPU1 and CPU2
    /// before the application starts them) looks the same as one that the group halted.
    running_peers: Vec<u8>,
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
            owns_triggers: false,
            halt_group: None,
            running_peers: Vec::new(),
        }
    }

    /// Whether this session has written the trigger units of the core.
    pub(crate) fn owns_triggers(&self) -> bool {
        self.owns_triggers
    }

    /// Whether this session put the core in the halt group.
    pub(crate) fn in_halt_group(&self) -> bool {
        self.halt_group.is_some()
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
        for (word, chunk) in data.iter_mut().zip(bytes.as_chunks::<8>().0) {
            *word = u64::from_le_bytes(*chunk);
        }
        Ok(())
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), TriCoreError> {
        check_alignment(address, 4)?;
        let mut bytes = vec![0; data.len() * 4];
        self.read_bytes(address, &mut bytes)?;
        for (word, chunk) in data.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *word = u32::from_le_bytes(*chunk);
        }
        Ok(())
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), TriCoreError> {
        check_alignment(address, 2)?;
        let mut bytes = vec![0; data.len() * 2];
        self.read_bytes(address, &mut bytes)?;
        for (word, chunk) in data.iter_mut().zip(bytes.as_chunks::<2>().0) {
            *word = u16::from_le_bytes(*chunk);
        }
        Ok(())
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), TriCoreError> {
        self.read_bytes(address, data)
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), TriCoreError> {
        check_alignment(address, 8)?;
        let bytes = data
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect::<Vec<_>>();
        self.write_bytes(address, &bytes)
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), TriCoreError> {
        check_alignment(address, 4)?;
        let bytes = data
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect::<Vec<_>>();
        self.write_bytes(address, &bytes)
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), TriCoreError> {
        check_alignment(address, 2)?;
        let bytes = data
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect::<Vec<_>>();
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
    /// The other TriCore cores of the device, as (core index, CSFR base). A run resumes the
    /// halt group members among them.
    peers: Vec<(u8, u32)>,
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
            peers: Vec::new(),
            sequence,
        }
    }

    /// Sets the other TriCore cores of the device, as (core index, CSFR base). The core itself
    /// is left out if it is in the list.
    pub fn with_peers(mut self, peers: impl IntoIterator<Item = (u8, u32)>) -> Self {
        let core = self.core();
        self.peers = peers.into_iter().filter(|&(i, _)| i != core).collect();
        self
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
        let bytes = values
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect::<Vec<_>>();
        self.bus().write_block(AddressMap::Target, address, &bytes)
    }

    /// Writes `DBGSR` between writes of the access hardware registers, in one request.
    ///
    /// The access hardware registers belong to the whole device. One request keeps another
    /// client of the probe, for example the debugger of another core, from running its own
    /// sequence in between.
    fn write_bracketed(&mut self, writes: &[(u32, u32)]) -> Result<(), TriCoreError> {
        let mut transfers = writes
            .iter()
            .map(|&(address, value)| BusTransfer::write_u32(AddressMap::Target, address, value))
            .collect::<Vec<_>>();
        self.bus().execute(&mut transfers)?;
        transfers.iter().try_for_each(BusTransfer::check)
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
        self.state.owns_triggers = true;
        Ok(())
    }

    /// Takes over the trigger units before the first step or breakpoint change of this session:
    /// triggers that an earlier session left behind are cleared.
    fn take_triggers(&mut self) -> Result<(), TriCoreError> {
        if !self.state.owns_triggers {
            self.reinit_triggers()?;
        }
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
    fn on_halted(&mut self, dbgsr: u32) -> Result<(), TriCoreError> {
        let trigger_acc = self.read_csfr(ocds::TRIG_ACC)? | self.state.pending_trigger_acc;
        self.state.pending_trigger_acc = 0;
        let pc = self.pc()?;

        // A trigger accumulator bit alone does not prove that a breakpoint fired: the boot
        // software and the step trap use TR0 too. An instruction breakpoint halts with the PC at
        // its address (break before make), so require that.
        let breakpoint_hit = self.state.breakpoints_enabled
            && self
                .state
                .breakpoints
                .iter()
                .enumerate()
                .any(|(unit, bp)| trigger_acc & (1 << unit) != 0 && *bp == Some(pc));

        let reason = if breakpoint_hit {
            HaltReason::Breakpoint(BreakpointCause::Hardware)
        } else {
            match self.state.last_action {
                LastAction::Step => HaltReason::Step,
                LastAction::Halt => HaltReason::Request,
                // Not halted by this session: the break-in of the halt group, or a halt request
                // of another session.
                LastAction::Run | LastAction::None if ocds::event_source(dbgsr) == 0 => {
                    HaltReason::External
                }
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
        let dbgsr = self.dbgsr()?;
        let halted = ocds::is_halted(dbgsr);
        if halted && !self.state.was_halted {
            self.on_halted(dbgsr)?;
        }
        if !halted {
            self.state.halt_reason = None;
            if self.state.was_halted {
                // Something else ran the core (the halt group, another session): the next halt is
                // not the one this session asked for.
                self.state.last_action = LastAction::Run;
            }
        }
        if self.state.halt_group.is_some() {
            self.check_group(halted)?;
        }
        self.state.was_halted = halted;
        Ok(halted)
    }

    /// [`Self::update_halted`] that rides out a device reboot: while another session resets the
    /// device, CSFR accesses fail for a short while.
    fn update_halted_through_reboot(&mut self) -> Result<bool, TriCoreError> {
        let start = Instant::now();
        loop {
            match self.update_halted() {
                Err(TriCoreError::TransferFailed { .. }) if start.elapsed() < REBOOT_TIME => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                result => return result,
            }
        }
    }

    /// For a core that this session put in the halt group: routes it again when a reset (by
    /// another session, for example) cleared the routing, and notes the members that run while
    /// this core runs (see `running_peers`).
    fn check_group(&mut self, halted: bool) -> Result<(), TriCoreError> {
        let group = self.read_group()?;
        if !ocds::in_halt_group(group.trc) || group.exevt != ocds::EXEVT_HALT {
            let csfr_base = self.options.csfr_base as u32;
            self.write_group_routing(self.core(), csfr_base)?;
        }
        if !halted {
            self.state.running_peers = group.running_members();
        }
        Ok(())
    }

    fn halt_core(&mut self, timeout: Duration) -> Result<(), TriCoreError> {
        if self.update_halted()? {
            return self.halt_group_members();
        }
        self.state.last_action = LastAction::Halt;
        self.write_csfr(ocds::DBGSR, ocds::DBGSR_HALT)?;

        let start = Instant::now();
        loop {
            if self.update_halted()? {
                return self.halt_group_members();
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
        self.take_triggers()?;

        self.step_debug_init(true)?;
        let dbgsr = self.csfr(ocds::DBGSR);
        self.write_bracketed(&[
            (ocds::ACCESS_HW_CTRL, ocds::ACCESS_HW_STEP_ON),
            (dbgsr, ocds::DBGSR_RUN),
            (ocds::ACCESS_HW_CTRL, ocds::ACCESS_HW_STEP_OFF),
        ])?;
        self.step_debug_init(false)?;
        // The debug init disabled the trigger units, restore the breakpoints.
        self.rearm_triggers()?;
        // It cleared EXEVT as well, which a halt group member needs to halt on break-in.
        if self.state.halt_group.is_some() {
            self.write_csfr(ocds::EXEVT, ocds::EXEVT_HALT)?;
        }

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
        // A run does not take over the triggers: a session that only resumes the core (for
        // example a command line tool) keeps the breakpoints of the session debugging it.
        if self.at_breakpoint()? {
            self.step_core()?;
        }

        self.state.last_action = LastAction::Run;
        // In the halt group, the members that the group halted run again with this core, in the
        // same request: a member that ran on alone could halt again before the others run, and
        // that halt would not reach them.
        let mut writes = Self::run_writes(self.csfr(ocds::DBGSR)).to_vec();
        for dbgsr in self.halted_group_members()? {
            writes.extend(Self::run_writes(dbgsr));
        }
        self.write_bracketed(&writes)?;

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

    /// The writes that run one core: a `DBGSR` run write between the access hardware writes.
    fn run_writes(dbgsr: u32) -> [(u32, u32); 5] {
        [
            (ocds::ACCESS_HW_CTRL, ocds::ACCESS_HW_STEP_ON),
            (dbgsr, ocds::DBGSR_RUN),
            (ocds::ACCESS_HW_CTRL, ocds::ACCESS_HW_STEP_OFF),
            (dbgsr, ocds::DBGSR_RUN),
            (ocds::ACCESS_HW_RUNCMD, 2),
        ]
    }

    /// Reads the `CBS_TRCn` and `EXEVT` of this core and the `CBS_TRCn` and `DBGSR` of its peers,
    /// in one request.
    fn read_group(&mut self) -> Result<GroupState, TriCoreError> {
        let mut transfers = vec![
            BusTransfer::read(AddressMap::Target, ocds::trc(self.core()), 4),
            BusTransfer::read(AddressMap::Target, self.csfr(ocds::EXEVT), 4),
        ];
        for &(core, csfr_base) in &self.peers {
            transfers.push(BusTransfer::read(AddressMap::Target, ocds::trc(core), 4));
            transfers.push(BusTransfer::read(
                AddressMap::Target,
                csfr_base + ocds::DBGSR as u32,
                4,
            ));
        }
        self.bus().execute(&mut transfers)?;
        let peers = self
            .peers
            .iter()
            .zip(transfers[2..].chunks(2))
            .map(|(&(core, _), pair)| Ok((core, pair[0].read_u32()?, pair[1].read_u32()?)))
            .collect::<Result<_, TriCoreError>>()?;
        Ok(GroupState {
            trc: transfers[0].read_u32()?,
            exevt: transfers[1].read_u32()?,
            peers,
        })
    }

    /// Notes the other halt group members that run now (see `running_peers`).
    fn note_running_peers(&mut self) -> Result<(), TriCoreError> {
        self.state.running_peers = self.read_group()?.running_members();
        Ok(())
    }

    fn peer_csfr_base(&self, core: u8) -> Option<u32> {
        self.peers
            .iter()
            .find(|&&(i, _)| i == core)
            .map(|&(_, csfr_base)| csfr_base)
    }

    /// The `DBGSR` addresses of the other halt group members to run with this core, when this
    /// session put the core in the group: the members that ran while this core last ran, and
    /// halted since. A member halted at one of its breakpoints stays halted: its own debugger
    /// steps over the breakpoint when it runs the core.
    fn halted_group_members(&mut self) -> Result<Vec<u32>, TriCoreError> {
        if self.state.halt_group.is_none() || self.state.running_peers.is_empty() {
            return Ok(Vec::new());
        }
        let peers = self.read_group()?.peers;
        let running_peers = std::mem::take(&mut self.state.running_peers);
        let halted = peers
            .into_iter()
            .filter(|&(core, trc, dbgsr)| {
                running_peers.contains(&core) && ocds::in_halt_group(trc) && ocds::is_halted(dbgsr)
            })
            .filter_map(|(core, _, _)| self.peer_csfr_base(core))
            .collect::<Vec<_>>();
        if halted.is_empty() {
            return Ok(halted);
        }

        // The PC and the trigger units of each halted member, in one request.
        let mut transfers = Vec::new();
        for &csfr_base in &halted {
            transfers.push(BusTransfer::read(
                AddressMap::Target,
                csfr_base + registers::CSFR_PC as u32,
                4,
            ));
            transfers.push(BusTransfer::read(
                AddressMap::Target,
                csfr_base + ocds::TRIG as u32,
                8 * ocds::TRIGGER_UNITS,
            ));
        }
        self.bus().execute(&mut transfers)?;
        let mut members = Vec::new();
        for (csfr_base, pair) in halted.into_iter().zip(transfers.chunks(2)) {
            let pc = pair[0].read_u32()?;
            pair[1].check()?;
            // Each trigger unit is `TRnEVT` followed by `TRnADR`.
            let at_breakpoint = pair[1].data.as_chunks::<8>().0.iter().any(|unit| {
                let [e0, e1, e2, e3, a0, a1, a2, a3] = *unit;
                ocds::is_halt_trigger(u32::from_le_bytes([e0, e1, e2, e3]))
                    && u32::from_le_bytes([a0, a1, a2, a3]) == pc
            });
            if !at_breakpoint {
                members.push(csfr_base + ocds::DBGSR as u32);
            }
        }
        Ok(members)
    }

    /// Halts the other halt group members that run: a member that stays halted keeps the group
    /// line active, so the halt of this core does not reach them through the line.
    fn halt_group_members(&mut self) -> Result<(), TriCoreError> {
        if self.state.halt_group.is_none() || self.peers.is_empty() {
            return Ok(());
        }
        let running = self
            .read_group()?
            .running_members()
            .into_iter()
            .filter_map(|core| self.peer_csfr_base(core))
            .collect::<Vec<_>>();
        let writes = running
            .into_iter()
            .map(|csfr_base| (csfr_base + ocds::DBGSR as u32, ocds::DBGSR_HALT))
            .collect::<Vec<_>>();
        if !writes.is_empty() {
            self.write_bracketed(&writes)?;
        }
        Ok(())
    }

    /// Writes the halt group routing of CPU `core` (`CBS_TRCn` and `EXEVT`), keeping the other
    /// `CBS_TRCn` fields.
    fn write_group_routing(&mut self, core: u8, csfr_base: u32) -> Result<(), TriCoreError> {
        let trc = self.bus().read_u32(AddressMap::Target, ocds::trc(core))?;
        let routed = (trc & !ocds::TRC_HALT_BRKIN) | ocds::TRC_HALT_GROUP;
        self.bus()
            .write_u32(AddressMap::Target, ocds::trc(core), routed)?;
        self.bus().write_u32(
            AddressMap::Target,
            csfr_base + ocds::EXEVT as u32,
            ocds::EXEVT_HALT,
        )
    }

    /// Puts the core in the halt group of the device (`join`), or takes it out.
    ///
    /// The members of the halt group halt together: when one halts (a breakpoint, a step, a
    /// halt request), the trigger switch line [`ocds::HALT_GROUP_LINE`] carries its halt state to
    /// the break-in of the others, and they halt as well. A run of a member also runs the members
    /// that the group halted.
    ///
    /// Leaving the group puts back the `CBS_TRCn` fields and `EXEVT` from before the core joined.
    pub fn set_halt_group(&mut self, join: bool) -> Result<(), TriCoreError> {
        let core = self.core();
        let trc_address = ocds::trc(core);
        if join {
            if self.state.halt_group.is_none() {
                let trc = self.bus().read_u32(AddressMap::Target, trc_address)?;
                let exevt = self.read_csfr(ocds::EXEVT)?;
                // A core that is in the group already (an earlier session left it there) leaves
                // it to the reset values.
                self.state.halt_group = Some(if ocds::in_halt_group(trc) {
                    (trc & !ocds::TRC_HALT_BRKIN, 0)
                } else {
                    (trc, exevt)
                });
            }
            // Joining while this core is halted halts the members that run now (a debugger halts
            // the core when it attaches): note them, so that the next run of this core runs them
            // again.
            self.note_running_peers()?;
            let csfr_base = self.options.csfr_base as u32;
            self.write_group_routing(core, csfr_base)
        } else if let Some((saved_trc, saved_exevt)) = self.state.halt_group.take() {
            let trc = self.bus().read_u32(AddressMap::Target, trc_address)?;
            let restored = (trc & !ocds::TRC_HALT_BRKIN) | (saved_trc & ocds::TRC_HALT_BRKIN);
            self.bus()
                .write_u32(AddressMap::Target, trc_address, restored)?;
            self.write_csfr(ocds::EXEVT, saved_exevt)
        } else {
            Ok(())
        }
    }

    fn reset_and_halt_core(&mut self, timeout: Duration) -> Result<(), TriCoreError> {
        self.bus().reset(ResetKind::ResetAndHalt)?;

        // The device reboots: CSFR accesses fail for a short while. Callers pass short timeouts
        // meant for a halt, so allow the reboot at least two seconds.
        let reboot_timeout = timeout.max(REBOOT_TIME);
        let start = Instant::now();
        loop {
            match self.dbgsr() {
                Ok(_) => break,
                Err(error) if start.elapsed() > reboot_timeout => return Err(error),
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        self.reinit_triggers()?;
        // The reset cleared the halt group routing. This core is routed again here, the members
        // of other sessions at their next status check.
        self.state.running_peers.clear();
        if self.state.halt_group.is_some() {
            let csfr_base = self.options.csfr_base as u32;
            self.write_group_routing(self.core(), csfr_base)?;
        }
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
        Ok(self.update_halted_through_reboot()?)
    }

    fn status(&mut self) -> Result<CoreStatus, Error> {
        if self.update_halted_through_reboot()? {
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
        self.take_triggers()?;
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
        self.take_triggers()?;
        self.state.breakpoints[unit_index] = Some(address);
        Ok(self.write_trigger(unit_index)?)
    }

    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), Error> {
        self.ensure_unit(unit_index)?;
        self.take_triggers()?;
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
