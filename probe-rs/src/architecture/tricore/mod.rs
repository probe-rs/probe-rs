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
    WatchpointAccess,
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
    /// Software debug event register: the debug action of the `DEBUG` instruction. The debug
    /// init writes [`SWEVT_HALT`] here.
    pub const SWEVT: u16 = 0xFD10;
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
    /// The [`SWEVT`] value of the debug init: a `DEBUG` instruction halts the core, break before
    /// make (the PC stays at the instruction).
    pub const SWEVT_HALT: u32 = 0x2A;

    /// `DBGSR.EVTSRC` after a halt by a `DEBUG` instruction.
    pub const EVTSRC_SWEVT: u32 = 2;

    /// `DBGSR.SUSP`: the suspend state that the last debug event set.
    pub const DBGSR_SUSP: u32 = 1 << 4;
    /// `DBGSR.PREVSUSP`: `SUSP` before the last debug event.
    pub const DBGSR_PREVSUSP: u32 = 1 << 6;

    /// `TRnEVT` of a data watchpoint without the access bits: address input (`TYP` = 0), halt,
    /// no break-out pulse, break after make, and `SUSP` set so that the halt can be told from
    /// the halt group's break-in that follows it (see [`halted_by_suspend_event`]).
    pub const TRIG_EVT_DATA: u32 = 0x32;
    /// `TRnEVT.ALD`: trigger on loads.
    pub const TRIG_EVT_LOAD: u32 = 1 << 28;
    /// `TRnEVT.AST`: trigger on stores.
    pub const TRIG_EVT_STORE: u32 = 1 << 27;
    /// `TRnEVT.RNG`: the unit and its odd neighbour form an address range.
    pub const TRIG_EVT_RANGE: u32 = 1 << 13;

    /// Whether a halted core's `DBGSR` shows a halt by a debug event with `SUSP` set: a data
    /// watchpoint or a `DEBUG` instruction. In the halt group the break-in right after such a
    /// halt overwrites `EVTSRC` (0) and moves `SUSP` to `PREVSUSP`; a run write clears `SUSP`.
    pub fn halted_by_suspend_event(dbgsr: u32) -> bool {
        let source = event_source(dbgsr);
        (source == 0 && dbgsr & DBGSR_PREVSUSP != 0) || (source != 0 && dbgsr & DBGSR_SUSP != 0)
    }

    /// The 16-bit `DEBUG` instruction.
    pub const DEBUG16: [u8; 2] = [0x00, 0xA0];
    /// The 32-bit `DEBUG` instruction.
    pub const DEBUG32: [u8; 4] = [0x0D, 0x00, 0x00, 0x01];

    /// The `DEBUG` instruction that replaces an instruction of `len` bytes (2 or 4).
    pub fn debug_instruction(len: usize) -> &'static [u8] {
        if len == 4 { &DEBUG32 } else { &DEBUG16 }
    }

    /// The length of the instruction that starts with byte `first`: bit 0 set means 32 bits.
    pub fn instruction_len(first: u8) -> usize {
        if first & 1 == 1 { 4 } else { 2 }
    }

    /// `TRnEVT` of the start of a performance counter window: PC input, break before make, no
    /// halt (`EVTA` 001 with `BOD`), `CNT` 01 (start the counters in task mode).
    pub const TRIG_EVT_PERF_START: u32 = 0x1059;
    /// `TRnEVT` of the end of a performance counter window: as the start, `CNT` 10 (stop).
    pub const TRIG_EVT_PERF_STOP: u32 = 0x1099;
    /// `CCTRL.CM`: task mode, the counters count between start and stop events.
    pub const CCTRL_CM: u32 = 1 << 0;
    /// `CCTRL.CE`: the counters are enabled.
    pub const CCTRL_CE: u32 = 1 << 1;
    /// `CCTRL.M1`-`M3`: the events that `M1CNT`-`M3CNT` count.
    pub const CCTRL_EVENTS: u32 = 0x7FC;

    /// STM0 `TIM0SV`, the second view of the low 32 bits of the system timer: reading it
    /// captures the high bits in `CAPSV` and leaves the application's `TIM0` / `CAP` pair alone.
    /// The system timer counts up from 0 after every application reset.
    pub const STM0_TIM0SV: u32 = 0xF000_0050;
    /// STM0 `CAPSV`: the system timer bits 63:32 captured by the last `TIM0SV` read.
    pub const STM0_CAPSV: u32 = 0xF000_0054;
    /// `SCU_RSTSTAT`: the triggers of the last reset.
    pub const SCU_RSTSTAT: u32 = 0xF003_6050;

    /// The reset triggers that `SCU_RSTSTAT` names in its low byte.
    pub fn reset_triggers(rststat: u32) -> Vec<&'static str> {
        [
            (0, "ESR0"),
            (1, "ESR1"),
            (3, "SMU (e.g. a watchdog)"),
            (4, "software"),
            (5, "STM0"),
            (6, "STM1"),
            (7, "STM2"),
        ]
        .into_iter()
        .filter(|&(bit, _)| rststat & (1 << bit) != 0)
        .map(|(_, name)| name)
        .collect()
    }

    /// `ICR.IE`: interrupts enabled.
    pub const ICR_IE: u32 = 1 << 15;

    /// Whether the instruction that starts with `code` (4 bytes, or 2 for a 16-bit instruction)
    /// changes `ICR.IE`: ENABLE, DISABLE, DISABLE D\[a\], RESTORE D\[a\], BISR, RFE, RFM, and
    /// MTCR to ICR. A step over one of them must not set `ICR.IE` back.
    pub fn changes_interrupt_enable(code: &[u8]) -> bool {
        let Some(&first) = code.first() else {
            return false;
        };
        if instruction_len(first) == 2 {
            let half = u16::from_le_bytes([first, code.get(1).copied().unwrap_or(0)]);
            // RFE (16 bit), BISR const8 (16 bit).
            return half == 0x8000 || first == 0xE0;
        }
        let Some(word) = code
            .get(..4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
        else {
            return false;
        };
        match first {
            // SYS format: ENABLE, DISABLE, DISABLE D[a], RESTORE D[a], RFM, RFE.
            0x0D => matches!((word >> 22) & 0x3F, 0x0C | 0x0D | 0x0F | 0x0E | 0x05 | 0x06),
            // RC format: BISR const9 (SYSCALL shares the opcode with op2 4).
            0xAD => (word >> 21) & 0x7F == 0,
            // RLC format: MTCR const16, D[a] with const16 = ICR.
            0xCD => {
                (word >> 12) & 0xFFFF == crate::architecture::tricore::registers::CSFR_ICR as u32
            }
            _ => false,
        }
    }

    /// The number of software breakpoints (`DEBUG` instructions) of a core.
    pub const SOFTWARE_BREAKPOINTS: usize = 32;
    /// The breakpoint slots of a core: the trigger units and the software breakpoints.
    pub const BREAKPOINT_SLOTS: usize = TRIGGER_UNITS + SOFTWARE_BREAKPOINTS;

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

/// The bus address (global alias) of `address` when it is in the PSPR of core `core_index`,
/// local or global. Software breakpoints are planted only there: no cache sits in front of it,
/// and only that core runs its code.
pub fn own_pspr_address(core_index: u8, address: u32) -> Option<u32> {
    let address = translate_local_address(core_index, address);
    let base = spr::PSPR_GLOBAL - core_index as u32 * spr::CORE_STEP;
    (base..base + spr::PSPR_SIZE)
        .contains(&address)
        .then_some(address)
}

/// A `DEBUG` instruction planted over an instruction in the PSPR of a core.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SoftwareBreakpoint {
    /// The bus address (global alias).
    address: u32,
    /// The instruction under the `DEBUG` instruction: 2 or 4 bytes.
    original: Vec<u8>,
}

impl SoftwareBreakpoint {
    fn debug(&self) -> &'static [u8] {
        ocds::debug_instruction(self.original.len())
    }

    /// The (offset in the breakpoint, offset in the access) of every byte that an access of
    /// `len` bytes at `address` shares with the breakpoint.
    fn overlap(&self, address: u32, len: usize) -> impl Iterator<Item = (usize, usize)> + use<> {
        let start = self.address as u64;
        let access = address as u64..address as u64 + len as u64;
        (0..self.original.len()).filter_map(move |i| {
            let byte = start + i as u64;
            access
                .contains(&byte)
                .then(|| (i, (byte - access.start) as usize))
        })
    }
}

/// A data watchpoint in one trigger unit (a single address) or a pair (`unit` even, `unit + 1`;
/// the range `[address, address + len)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Watchpoint {
    address: u32,
    len: u32,
    access: WatchpointAccess,
    unit: usize,
}

impl Watchpoint {
    fn is_range(&self) -> bool {
        self.len > 1
    }

    fn uses(&self, unit: usize) -> bool {
        unit == self.unit || (self.is_range() && unit == self.unit + 1)
    }

    /// The `TRnEVT` value of the first unit.
    fn trigger_evt(&self) -> u32 {
        let access = match self.access {
            WatchpointAccess::Read => ocds::TRIG_EVT_LOAD,
            WatchpointAccess::Write => ocds::TRIG_EVT_STORE,
            WatchpointAccess::ReadWrite => ocds::TRIG_EVT_LOAD | ocds::TRIG_EVT_STORE,
        };
        let range = if self.is_range() {
            ocds::TRIG_EVT_RANGE
        } else {
            0
        };
        ocds::TRIG_EVT_DATA | access | range
    }
}

/// A performance counter window (A-to-B): the counters count from `start` to `stop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PerfWindow {
    start: u32,
    stop: u32,
    /// The trigger units of the start and the stop.
    units: [usize; 2],
    /// `CCTRL` from before the window, put back when it is cleared.
    saved_cctrl: u32,
}

/// How long CSFR accesses may fail while the device reboots after a reset.
const REBOOT_TIME: Duration = Duration::from_secs(2);

/// How long after a reset of the device the status checks plant the software breakpoints again
/// where the program's start code copied code over them.
const RESET_SETTLE_TIME: Duration = Duration::from_secs(3);

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
    /// The breakpoint addresses, per slot that the debugger sees. A breakpoint takes a trigger
    /// unit (`breakpoints`); one in the PSPR of the core is a `DEBUG` instruction (`software`)
    /// when no unit is free (see `TriCore::place`).
    slots: Vec<Option<u32>>,
    /// The hardware breakpoints, per trigger unit.
    breakpoints: [Option<u32>; ocds::TRIGGER_UNITS],
    /// The planted software breakpoints. Lent to [`TriCoreMemory`] while a [`TriCore`] exists.
    software: Vec<SoftwareBreakpoint>,
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
    /// Whether a single step runs with interrupts disabled: a pending interrupt would otherwise
    /// end the step on the first instruction of its handler.
    mask_step_interrupts: bool,
    /// The data watchpoints, in trigger units that no breakpoint uses.
    watchpoints: Vec<Watchpoint>,
    /// The performance counter window, in two trigger units.
    perf_window: Option<PerfWindow>,
    /// The system timer at the last status check. It counting down tells a reset that this
    /// session did not make (an application reset: software, watchdog, ...).
    last_stm: Option<u64>,
    /// Whether the core halts when such a reset is seen. Otherwise it runs on.
    halt_after_reset: bool,
    /// Set while the core halts because of such a reset, for the halt reason.
    reset_halt: bool,
    /// When such a reset was seen. For a while after it, the status checks plant the software
    /// breakpoints again where the program copied its code over them.
    reset_seen: Option<Instant>,
    /// Whether a breakpoint in the PSPR of the core is a `DEBUG` instruction even when a trigger
    /// unit is free. By default it takes a free unit, which a reset of the device and a program
    /// that copies its code into the PSPR leave alone; `DEBUG` instructions are for the
    /// breakpoints beyond the units.
    software_first: bool,
    /// Messages for the user (a reset was seen), taken by [`Self::take_notices`].
    notices: Vec<String>,
}

impl TriCoreCoreState {
    /// Creates the state of a core that was not attached yet.
    pub(crate) fn new() -> Self {
        Self {
            slots: vec![None; ocds::BREAKPOINT_SLOTS],
            breakpoints: [None; ocds::TRIGGER_UNITS],
            software: Vec::new(),
            breakpoints_enabled: false,
            was_halted: false,
            halt_reason: None,
            pending_trigger_acc: 0,
            last_action: LastAction::None,
            owns_triggers: false,
            halt_group: None,
            running_peers: Vec::new(),
            mask_step_interrupts: true,
            watchpoints: Vec::new(),
            perf_window: None,
            last_stm: None,
            halt_after_reset: false,
            reset_halt: false,
            reset_seen: None,
            software_first: false,
            notices: Vec::new(),
        }
    }

    /// Makes breakpoints in the PSPR of the core `DEBUG` instructions even when a trigger unit
    /// is free (see `software_first`).
    #[cfg(test)]
    pub(crate) fn set_software_first(&mut self, software_first: bool) {
        self.software_first = software_first;
    }

    /// Takes the messages for the user that the core collected since the last call (for
    /// example a reset of the device). A debugger shows them in its console.
    pub(crate) fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    /// Sets whether the core halts when the device was reset by something other than this
    /// session (the default is to let it run on).
    pub(crate) fn set_halt_after_reset(&mut self, halt: bool) {
        self.halt_after_reset = halt;
    }

    /// Whether a breakpoint, a watchpoint or the performance counter window uses trigger unit
    /// `unit`.
    fn unit_used(&self, unit: usize) -> bool {
        self.breakpoints[unit].is_some()
            || self.watchpoints.iter().any(|w| w.uses(unit))
            || self.perf_window.is_some_and(|p| p.units.contains(&unit))
    }

    /// Whether this session set a performance counter window on the core.
    pub(crate) fn has_perf_window(&self) -> bool {
        self.perf_window.is_some()
    }

    /// Sets whether single steps run with interrupts disabled (the default).
    pub(crate) fn set_mask_step_interrupts(&mut self, mask: bool) {
        self.mask_step_interrupts = mask;
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
/// Local scratchpad addresses are translated to the global aliases of the core. The planted
/// software breakpoints are hidden: reads return the instructions under them, and writes over
/// them change those instructions and keep the breakpoints.
#[derive(Debug)]
pub struct TriCoreMemory<'probe> {
    bus: &'probe mut dyn TriCoreBusAccess,
    core_index: u8,
    /// The planted software breakpoints, lent by the core state while a [`TriCore`] exists.
    software: Vec<SoftwareBreakpoint>,
}

impl<'probe> TriCoreMemory<'probe> {
    /// Creates the memory interface of core `core_index`.
    pub fn new(bus: &'probe mut dyn TriCoreBusAccess, core_index: u8) -> Self {
        Self {
            bus,
            core_index,
            software: Vec::new(),
        }
    }

    fn address(&self, address: u64) -> Result<u32, TriCoreError> {
        let address = u32::try_from(address).map_err(|_| {
            TriCoreError::Protocol(format!("address {address:#x} is outside the 32-bit space"))
        })?;
        Ok(translate_local_address(self.core_index, address))
    }

    fn read_bytes(&mut self, address: u64, data: &mut [u8]) -> Result<(), TriCoreError> {
        let address = self.address(address)?;
        self.bus.read_block(AddressMap::Target, address, data)?;
        for sw in &self.software {
            let debug = sw.debug();
            for (i, j) in sw.overlap(address, data.len()) {
                // Code written over the breakpoint by other means is shown as it is.
                if data[j] == debug[i] {
                    data[j] = sw.original[i];
                }
            }
        }
        Ok(())
    }

    fn write_bytes(&mut self, address: u64, data: &[u8]) -> Result<(), TriCoreError> {
        let address = self.address(address)?;
        let mut data = std::borrow::Cow::Borrowed(data);
        for sw in &mut self.software {
            let debug = sw.debug();
            for (i, j) in sw.overlap(address, data.len()) {
                sw.original[i] = data[j];
                data.to_mut()[j] = debug[i];
            }
        }
        self.bus.write_block(AddressMap::Target, address, &data)
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
        let mut memory = TriCoreMemory::new(bus, options.core_index);
        memory.software = std::mem::take(&mut state.software);
        Self {
            memory,
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

    /// Writes one trigger unit from the state: armed when a breakpoint or a watchpoint uses it
    /// and breakpoints are enabled, disabled otherwise. The second unit of a watchpoint range
    /// only holds the end address.
    fn write_trigger(&mut self, unit: usize) -> Result<(), TriCoreError> {
        let enabled = self.state.breakpoints_enabled;
        let watchpoint = self
            .state
            .watchpoints
            .iter()
            .find(|w| w.uses(unit))
            .copied();
        match (self.state.breakpoints[unit], watchpoint) {
            (Some(address), _) if enabled => {
                self.write_csfr(Self::trigger_adr(unit), address)?;
                self.write_csfr(Self::trigger_evt(unit), ocds::TRIG_EVT_IP)
            }
            (None, Some(w)) if enabled && w.unit == unit => {
                self.write_csfr(Self::trigger_adr(unit), w.address)?;
                self.write_csfr(Self::trigger_evt(unit), w.trigger_evt())
            }
            (None, Some(w)) if enabled => {
                self.write_csfr(Self::trigger_adr(unit), w.address.wrapping_add(w.len))?;
                self.write_csfr(Self::trigger_evt(unit), 0)
            }
            (None, None) => match self.state.perf_window {
                // The window counts whether breakpoints are enabled or not.
                Some(p) if p.units[0] == unit => {
                    self.write_csfr(Self::trigger_adr(unit), p.start)?;
                    self.write_csfr(Self::trigger_evt(unit), ocds::TRIG_EVT_PERF_START)
                }
                Some(p) if p.units[1] == unit => {
                    self.write_csfr(Self::trigger_adr(unit), p.stop)?;
                    self.write_csfr(Self::trigger_evt(unit), ocds::TRIG_EVT_PERF_STOP)
                }
                _ => self.write_csfr(Self::trigger_evt(unit), 0),
            },
            _ => self.write_csfr(Self::trigger_evt(unit), 0),
        }
    }

    /// Sets the performance counter window (A-to-B): the counters (`CCNT`, `ICNT`,
    /// `M1CNT`-`M3CNT`) are cleared and from then on count only from an execution of `start` to
    /// one of `stop`, each time. `None` clears the window and puts `CCTRL` back.
    ///
    /// The window takes two trigger units (the highest free ones), which do not halt the core.
    pub fn set_perf_window(&mut self, window: Option<(u32, u32)>) -> Result<(), TriCoreError> {
        self.take_triggers()?;
        if let Some(old) = self.state.perf_window.take() {
            for unit in old.units {
                self.write_trigger(unit)?;
            }
            self.write_csfr(registers::CSFR_CCTRL, old.saved_cctrl)?;
        }
        let Some((start, stop)) = window else {
            return self.promote_software();
        };
        // Any two units, the highest first.
        let candidates = (0..ocds::TRIGGER_UNITS)
            .rev()
            .flat_map(|high| (0..high).rev().map(move |low| vec![high, low]))
            .collect::<Vec<_>>();
        let claimed = self.claim_units(&candidates)?.unwrap_or_default();
        let [stop_unit, start_unit] = claimed[..] else {
            return Err(TriCoreError::Protocol(format!(
                "no two free trigger units for the performance counter window (the {} units \
                 are in use)",
                ocds::TRIGGER_UNITS
            )));
        };
        let saved_cctrl = self.read_csfr(registers::CSFR_CCTRL)?;
        // The counters can be written only while they are disabled.
        self.write_csfr(registers::CSFR_CCTRL, 0)?;
        self.write_csfr_words(registers::CSFR_CCNT, &[0; 5])?;
        self.state.perf_window = Some(PerfWindow {
            start,
            stop,
            units: [start_unit, stop_unit],
            saved_cctrl,
        });
        self.write_trigger(start_unit)?;
        self.write_trigger(stop_unit)?;
        self.write_csfr(
            registers::CSFR_CCTRL,
            (saved_cctrl & ocds::CCTRL_EVENTS) | ocds::CCTRL_CM | ocds::CCTRL_CE,
        )
    }

    /// Rewrites the trigger units in use. The single step sequence reprograms the trigger
    /// window, so the breakpoints and watchpoints are restored after it.
    fn rearm_triggers(&mut self) -> Result<(), TriCoreError> {
        for unit in 0..ocds::TRIGGER_UNITS {
            if self.state.unit_used(unit) {
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
        self.write_csfr(ocds::SWEVT, ocds::SWEVT_HALT)?;
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
        // A `DEBUG` instruction: one of the software breakpoints, or one in the program. In the
        // halt group the break-in that follows the halt overwrites `EVTSRC`, so a software
        // breakpoint is told by the PC.
        let debug_instruction = ocds::event_source(dbgsr) == ocds::EVTSRC_SWEVT
            || (self.software_at(pc).is_some() && self.state.last_action != LastAction::Halt);
        // A data watchpoint halts after the access: its trigger accumulator bit tells it.
        let watchpoint_hit = self.state.breakpoints_enabled
            && self.state.watchpoints.iter().any(|w| {
                (0..ocds::TRIGGER_UNITS).any(|unit| w.uses(unit) && trigger_acc & (1 << unit) != 0)
            });

        let reason = if std::mem::take(&mut self.state.reset_halt) {
            // Halted after a reset of the device (`resetBehavior: halt`).
            HaltReason::Exception
        } else if breakpoint_hit {
            HaltReason::Breakpoint(BreakpointCause::Hardware)
        } else if watchpoint_hit {
            HaltReason::Watchpoint
        } else if debug_instruction {
            HaltReason::Breakpoint(BreakpointCause::Software)
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

    /// `DBGSR` and the system timer (when it can be read), in one request.
    fn read_status(&mut self) -> Result<(u32, Option<u64>), TriCoreError> {
        let mut transfers = [
            BusTransfer::read(AddressMap::Target, self.csfr(ocds::DBGSR), 4),
            BusTransfer::read(AddressMap::Target, ocds::STM0_TIM0SV, 4),
            BusTransfer::read(AddressMap::Target, ocds::STM0_CAPSV, 4),
        ];
        self.bus().execute(&mut transfers)?;
        let dbgsr = transfers[0].read_u32()?;
        let stm = match (transfers[1].read_u32(), transfers[2].read_u32()) {
            (Ok(low), Ok(high)) => Some((high as u64) << 32 | low as u64),
            _ => None,
        };
        Ok((dbgsr, stm))
    }

    /// Called when the system timer counted down since the last status check: the device was
    /// reset, not by this session (software, watchdog or SMU, a pin, another session). The
    /// debug settings of the cores survive such a reset; the program may have loaded its code
    /// into the PSPR again, over the software breakpoints.
    fn on_device_reset(&mut self) -> Result<(), TriCoreError> {
        let rststat = self.bus().read_u32(AddressMap::Target, ocds::SCU_RSTSTAT)?;
        let triggers = ocds::reset_triggers(rststat);
        let cause = if triggers.is_empty() {
            format!("RSTSTAT {rststat:#010x}")
        } else {
            format!("{} (RSTSTAT {rststat:#010x})", triggers.join(", "))
        };
        let notice = format!(
            "TriCore core {}: the device was reset by {cause}. {}",
            self.core(),
            if self.state.halt_after_reset {
                "The core is halted (`resetBehavior: halt`)."
            } else {
                "Breakpoints are kept and the core runs on."
            }
        );
        tracing::warn!("{notice}");
        self.state.notices.push(notice);
        self.state.running_peers.clear();
        self.state.was_halted = false;
        self.state.last_action = LastAction::Run;
        if self.state.owns_triggers {
            self.rearm_triggers()?;
        }
        // The units compare the PC: copied code does not take them away.
        self.promote_software()?;
        self.replant_software()?;
        self.state.reset_seen = Some(Instant::now());
        if self.state.halt_after_reset {
            self.state.reset_halt = true;
            self.write_csfr(ocds::DBGSR, ocds::DBGSR_HALT)?;
        }
        Ok(())
    }

    /// Plants the software breakpoints again where the code under them changed (the program
    /// copied its code into the PSPR again): the new code is the instruction under them.
    fn replant_software(&mut self) -> Result<(), TriCoreError> {
        if self.memory.software.is_empty() {
            return Ok(());
        }
        // The aligned words around each breakpoint (it is halfword aligned), in one request: 32
        // bit reads need an aligned address.
        let words = |sw: &SoftwareBreakpoint| {
            let start = sw.address & !3;
            let end = (sw.address + sw.original.len() as u32 + 3) & !3;
            (start..end).step_by(4)
        };
        let mut reads = self
            .memory
            .software
            .iter()
            .flat_map(words)
            .map(|word| BusTransfer::read(AddressMap::Target, word, 4))
            .collect::<Vec<_>>();
        self.bus().execute(&mut reads)?;
        let mut changed = Vec::new();
        let mut next = reads.iter();
        for (i, sw) in self.memory.software.iter().enumerate() {
            let mut bytes = Vec::new();
            for read in next.by_ref().take(words(sw).count()) {
                read.check()?;
                bytes.extend_from_slice(&read.data);
            }
            let offset = (sw.address & 3) as usize;
            let code = bytes[offset..offset + sw.original.len()].to_vec();
            if code != sw.debug() {
                changed.push((i, code));
            }
        }
        if changed.is_empty() {
            return Ok(());
        }
        self.write_csfr(ocds::SWEVT, ocds::SWEVT_HALT)?;
        for (i, code) in changed {
            tracing::debug!(
                "TriCore core {}: the code under the software breakpoint at {:#010x} changed, \
                 planting it again",
                self.core(),
                self.memory.software[i].address
            );
            self.memory.software[i].original = code;
            let sw = self.memory.software[i].clone();
            self.bus()
                .write_block(AddressMap::Target, sw.address, sw.debug())?;
        }
        Ok(())
    }

    fn update_halted(&mut self) -> Result<bool, TriCoreError> {
        let (mut dbgsr, stm) = self.read_status()?;
        if let Some(stm) = stm {
            let reset = self.state.last_stm.is_some_and(|last| stm < last);
            self.state.last_stm = Some(stm);
            if reset {
                self.on_device_reset()?;
                dbgsr = self.dbgsr()?;
            } else if let Some(seen) = self.state.reset_seen {
                // The start code may copy its code into the PSPR after the reset was seen, over
                // the software breakpoints planted then.
                if seen.elapsed() < RESET_SETTLE_TIME {
                    self.replant_software()?;
                } else {
                    self.state.reset_seen = None;
                }
            }
        }
        let halted = ocds::is_halted(dbgsr);
        if halted && !self.state.was_halted {
            self.on_halted(dbgsr)?;
            // A member that ran when this core halted missed the halt on the group line (it was
            // still halted itself, or resumed just after): halt it now.
            self.halt_group_members()?;
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

    /// Whether the core is halted at a planted breakpoint. Running from there would hit the same
    /// trigger or `DEBUG` instruction again, so the instruction is stepped first.
    fn at_breakpoint(&mut self) -> Result<bool, TriCoreError> {
        if !self.state.breakpoints_enabled || self.state.slots.iter().all(Option::is_none) {
            return Ok(false);
        }
        let pc = self.pc()?;
        Ok(self.state.breakpoints.contains(&Some(pc)) || self.software_at(pc).is_some())
    }

    /// The index of the software breakpoint at CPU address `address`.
    fn software_at(&self, address: u32) -> Option<usize> {
        let address = translate_local_address(self.core(), address);
        self.memory
            .software
            .iter()
            .position(|sw| sw.address == address)
    }

    /// Moves the PC past a `DEBUG` instruction of the program that halted the core (break before
    /// make leaves the PC at it, so running would halt there again). Returns whether it did.
    fn skip_program_debug(&mut self, dbgsr: u32, pc: u32) -> Result<bool, TriCoreError> {
        if ocds::event_source(dbgsr) != ocds::EVTSRC_SWEVT || self.software_at(pc).is_some() {
            return Ok(false);
        }
        let address = translate_local_address(self.core(), pc);
        let mut code = [0; 4];
        self.bus()
            .read_block(AddressMap::Target, address, &mut code[..2])?;
        let len = ocds::instruction_len(code[0]);
        if len == 4 {
            self.bus()
                .read_block(AddressMap::Target, address, &mut code)?;
        }
        if code[..len] != *ocds::debug_instruction(len) {
            return Ok(false);
        }
        self.write_csfr(registers::CSFR_PC, pc + len as u32)?;
        Ok(true)
    }

    fn step_core(&mut self) -> Result<(), TriCoreError> {
        let dbgsr = self.dbgsr()?;
        if !ocds::is_halted(dbgsr) {
            return Err(TriCoreError::NotHalted(self.core()));
        }
        self.take_triggers()?;

        let pc = self.pc()?;
        if self.skip_program_debug(dbgsr, pc)? {
            // Moving past the `DEBUG` instruction is the step.
            self.state.last_action = LastAction::Step;
            self.state.halt_reason = Some(HaltReason::Step);
            self.state.was_halted = true;
            return Ok(());
        }
        // The instruction under a software breakpoint at the PC runs, the breakpoint is planted
        // again after the step.
        let lifted = self
            .software_at(pc)
            .map(|i| self.memory.software[i].clone());
        if let Some(sw) = &lifted {
            self.bus()
                .write_block(AddressMap::Target, sw.address, &sw.original)?;
        }
        let masked = self.mask_interrupts_for_step(pc)?;
        let stepped = self.step_once();
        if masked {
            // The instruction left ICR.IE alone (see `mask_interrupts_for_step`).
            let icr = self.read_csfr(registers::CSFR_ICR)?;
            self.write_csfr(registers::CSFR_ICR, icr | ocds::ICR_IE)?;
        }
        if let Some(sw) = &lifted {
            self.bus()
                .write_block(AddressMap::Target, sw.address, sw.debug())?;
        }
        stepped
    }

    /// Clears `ICR.IE` for a single step, when interrupts are enabled, the session masks them
    /// during steps and the instruction at `pc` does not change `ICR.IE` itself. Returns whether
    /// it did: the caller sets `ICR.IE` again after the step.
    fn mask_interrupts_for_step(&mut self, pc: u32) -> Result<bool, TriCoreError> {
        if !self.state.mask_step_interrupts {
            return Ok(false);
        }
        let icr = self.read_csfr(registers::CSFR_ICR)?;
        if icr & ocds::ICR_IE == 0 {
            return Ok(false);
        }
        let address = translate_local_address(self.core(), pc);
        let mut code = [0; 4];
        self.bus()
            .read_block(AddressMap::Target, address, &mut code[..2])?;
        if ocds::instruction_len(code[0]) == 4 {
            self.bus()
                .read_block(AddressMap::Target, address, &mut code)?;
        }
        if ocds::changes_interrupt_enable(&code) {
            return Ok(false);
        }
        self.write_csfr(registers::CSFR_ICR, icr & !ocds::ICR_IE)?;
        Ok(true)
    }

    /// The single step sequence of [`Self::step_core`].
    fn step_once(&mut self) -> Result<(), TriCoreError> {
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
        let dbgsr = self.dbgsr()?;
        if !ocds::is_halted(dbgsr) {
            // Already running.
            return Ok(());
        }
        // Code written over the software breakpoints by other means while the core was halted
        // (another tool loaded the program, start code after a reset): plant them again.
        self.replant_software()?;
        // A run does not take over the triggers: a session that only resumes the core (for
        // example a command line tool) keeps the breakpoints of the session debugging it.
        let pc = self.pc()?;
        if !self.skip_program_debug(dbgsr, pc)? && self.at_breakpoint()? {
            self.step_core()?;
        }

        self.state.last_action = LastAction::Run;
        // In the halt group, the members that the group halted run again with this core, in the
        // same request: a member that ran on alone could halt again before the others run, and
        // that halt would not reach them.
        // This core runs last: it is the one the user continues, often close to a breakpoint, and
        // a halt before the others run would not reach them.
        let mut writes = Vec::new();
        for dbgsr in self.halted_group_members()? {
            writes.extend(Self::run_writes(dbgsr));
        }
        let peers_run = !writes.is_empty();
        writes.extend(Self::run_writes(self.csfr(ocds::DBGSR)));
        self.write_bracketed(&writes)?;

        let dbgsr = self.dbgsr()?;
        if !ocds::is_running(dbgsr) {
            if peers_run {
                // Halted again before the request ran the others.
                self.halt_group_members()?;
            }
            // A trigger or `DEBUG` instruction that catches the core right after the start is a
            // normal halt: keep the trigger accumulator bits for the halt reason.
            let trigger_acc = self.read_csfr(ocds::TRIG_ACC)?;
            if trigger_acc == 0 && ocds::event_source(dbgsr) != ocds::EVTSRC_SWEVT {
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
    /// halted since. A member halted at one of its breakpoints (a trigger unit or a `DEBUG`
    /// instruction) stays halted: its own debugger steps over the breakpoint when it runs the
    /// core.
    ///
    /// The members that run now and the ones returned become the `running_peers` of the run: the
    /// core may halt again before a status check sees it running.
    fn halted_group_members(&mut self) -> Result<Vec<u32>, TriCoreError> {
        if self.state.halt_group.is_none() || self.peers.is_empty() {
            return Ok(Vec::new());
        }
        let group = self.read_group()?;
        let previous = std::mem::take(&mut self.state.running_peers);
        self.state.running_peers = group.running_members();
        let to_run = self.members_to_run(&group, &previous)?;
        self.state
            .running_peers
            .extend(to_run.iter().map(|&(core, _)| core));
        Ok(to_run.into_iter().map(|(_, dbgsr)| dbgsr).collect())
    }

    /// The (core index, `DBGSR` address) of the members for [`Self::halted_group_members`].
    fn members_to_run(
        &mut self,
        group: &GroupState,
        previous: &[u8],
    ) -> Result<Vec<(u8, u32)>, TriCoreError> {
        let halted = group
            .peers
            .iter()
            .filter(|&&(core, trc, dbgsr)| {
                previous.contains(&core) && ocds::in_halt_group(trc) && ocds::is_halted(dbgsr)
                    // Halted at a data watchpoint (or a `DEBUG` instruction).
                    && !ocds::halted_by_suspend_event(dbgsr)
            })
            .filter_map(|&(core, _, _)| Some((core, self.peer_csfr_base(core)?)))
            .collect::<Vec<_>>();
        if halted.is_empty() {
            return Ok(Vec::new());
        }

        // The PC and the trigger units of each halted member, in one request.
        let mut transfers = Vec::new();
        for &(_, csfr_base) in &halted {
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
        let mut candidates = Vec::new();
        for (&(core, csfr_base), pair) in halted.iter().zip(transfers.chunks(2)) {
            let pc = pair[0].read_u32()?;
            pair[1].check()?;
            // Each trigger unit is `TRnEVT` followed by `TRnADR`.
            let at_breakpoint = pair[1].data.as_chunks::<8>().0.iter().any(|unit| {
                let [e0, e1, e2, e3, a0, a1, a2, a3] = *unit;
                ocds::is_halt_trigger(u32::from_le_bytes([e0, e1, e2, e3]))
                    && u32::from_le_bytes([a0, a1, a2, a3]) == pc
            });
            if !at_breakpoint {
                candidates.push((core, translate_local_address(core, pc), csfr_base));
            }
        }
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        // A member halted at a `DEBUG` instruction (a software breakpoint) stays halted as well.
        // The two aligned words from the PC (halfword aligned) hold the instruction: 32 bit
        // reads need an aligned address.
        let mut transfers = candidates
            .iter()
            .flat_map(|&(_, pc, _)| {
                let word = pc & !3;
                [word, word + 4].map(|address| BusTransfer::read(AddressMap::Target, address, 4))
            })
            .collect::<Vec<_>>();
        self.bus().execute(&mut transfers)?;
        let members = candidates
            .into_iter()
            .zip(transfers.chunks(2))
            .filter(|((_, pc, _), words)| {
                // Code at the end of a memory may not have the second word: then it is no
                // `DEBUG32` there.
                let mut bytes = Vec::new();
                for word in *words {
                    if word.check().is_err() {
                        break;
                    }
                    bytes.extend_from_slice(&word.data);
                }
                let code = bytes.get((pc & 3) as usize..).unwrap_or_default();
                !code.starts_with(&ocds::DEBUG16) && !code.starts_with(&ocds::DEBUG32)
            })
            .map(|((core, _, csfr_base), _)| (core, csfr_base + ocds::DBGSR as u32))
            .collect();
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

        // The device reboots: CSFR accesses fail for a short while, and an access that worked
        // once can fail again (the device may reboot a second time). The whole sequence after
        // the reset is repeated until it gets through. Callers pass short timeouts meant for a
        // halt, so allow the reboot at least two seconds.
        let reboot_timeout = timeout.max(REBOOT_TIME);
        let start = Instant::now();
        loop {
            // This session's own reset is not reported as a reset of the device.
            self.state.last_stm = None;
            match self.finish_reset(timeout) {
                Err(error) if start.elapsed() > reboot_timeout => return Err(error),
                Err(error) => {
                    tracing::debug!("TriCore core {}: rebooting ({error})", self.core());
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(()) => {
                    self.state.last_stm = None;
                    return Ok(());
                }
            }
        }
    }

    /// The sequence after a reset of [`Self::reset_and_halt_core`].
    fn finish_reset(&mut self, timeout: Duration) -> Result<(), TriCoreError> {
        self.dbgsr()?;
        self.reinit_triggers()?;
        // The reset cleared SWEVT as well: the planted `DEBUG` instructions would not halt.
        if !self.memory.software.is_empty() {
            self.write_csfr(ocds::SWEVT, ocds::SWEVT_HALT)?;
        }
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

    fn ensure_slot(&self, slot: usize) -> Result<(), TriCoreError> {
        if slot >= ocds::BREAKPOINT_SLOTS {
            return Err(TriCoreError::BreakpointOutOfBounds(slot));
        }
        Ok(())
    }

    fn trigger_unit_of(&self, address: u32) -> Option<usize> {
        self.state
            .breakpoints
            .iter()
            .position(|&bp| bp == Some(address))
    }

    /// Sets the breakpoint of `slot` to `address`.
    fn set_breakpoint(&mut self, slot: usize, address: u32) -> Result<(), Error> {
        self.ensure_slot(slot)?;
        self.take_triggers()?;
        if self.state.slots[slot].is_some_and(|old| old != address) {
            self.clear_breakpoint(slot)?;
        }
        self.state.slots[slot] = Some(address);
        self.place(slot, address)
    }

    /// Puts the breakpoint of `slot` at `address` in place, in a trigger unit or as a `DEBUG`
    /// instruction (only in the PSPR of the core).
    ///
    /// A breakpoint in the PSPR takes a free trigger unit; without one it is a `DEBUG`
    /// instruction, which waits while breakpoints are disabled. A breakpoint elsewhere needs a
    /// unit: when none is free, a breakpoint in the PSPR gives its unit up for a `DEBUG`
    /// instruction.
    fn place(&mut self, slot: usize, address: u32) -> Result<(), Error> {
        if let Some(unit) = self.trigger_unit_of(address) {
            return Ok(self.write_trigger(unit)?);
        }
        if self.software_at(address).is_some() {
            return Ok(());
        }
        let pspr = own_pspr_address(self.core(), address);
        if let Some(bus_address) = pspr
            && self.state.software_first
        {
            if !self.state.breakpoints_enabled {
                return Ok(());
            }
            match self.plant(bus_address) {
                Ok(()) => return Ok(()),
                Err(error) => tracing::warn!(
                    "TriCore core {}: no software breakpoint at {address:#010x} ({error}), \
                     using a trigger unit",
                    self.core()
                ),
            }
        }
        // The trigger unit of the same number as the slot, else the lowest one.
        let candidates = (slot < ocds::TRIGGER_UNITS)
            .then_some(slot)
            .into_iter()
            .chain(0..ocds::TRIGGER_UNITS)
            .map(|unit| vec![unit])
            .collect::<Vec<_>>();
        // Taking the unit of another breakpoint in the PSPR would only swap the two.
        let unit = if pspr.is_some() {
            candidates
                .iter()
                .find(|units| !self.state.unit_used(units[0]))
                .map(|units| units[0])
        } else {
            self.claim_units(&candidates)?.map(|units| units[0])
        };
        match (unit, pspr) {
            (Some(unit), _) => {
                self.state.breakpoints[unit] = Some(address);
                Ok(self.write_trigger(unit)?)
            }
            (None, Some(_)) if !self.state.breakpoints_enabled => Ok(()),
            (None, Some(bus_address)) => self.plant(bus_address).map_err(|error| {
                self.state.slots[slot] = None;
                Error::Other(format!(
                    "No breakpoint at {address:#010x}: all {} trigger units are in use, and no \
                     DEBUG instruction could be planted ({error})",
                    ocds::TRIGGER_UNITS
                ))
            }),
            (None, None) => {
                self.state.slots[slot] = None;
                Err(Error::Other(format!(
                    "No free hardware breakpoint for {address:#010x}: all {} trigger units are \
                     in use (software breakpoints work only in the PSPR of the core)",
                    ocds::TRIGGER_UNITS
                )))
            }
        }
    }

    /// Whether the breakpoint in trigger unit `unit` can give the unit up for a `DEBUG`
    /// instruction: it is in the PSPR of the core.
    fn movable_to_software(&self, unit: usize) -> bool {
        !self.state.software_first
            && self.state.breakpoints[unit]
                .is_some_and(|address| own_pspr_address(self.core(), address).is_some())
    }

    /// Frees units for a new trigger: the first of `candidates` (each a set of units) whose
    /// units are all free, else the first whose units are free or hold breakpoints that can move
    /// to `DEBUG` instructions, which they then do. `None` if no candidate can be freed.
    fn claim_units(
        &mut self,
        candidates: &[Vec<usize>],
    ) -> Result<Option<Vec<usize>>, TriCoreError> {
        let free = |this: &Self, unit: usize| !this.state.unit_used(unit);
        let Some(units) = candidates
            .iter()
            .find(|units| units.iter().all(|&unit| free(self, unit)))
            .or_else(|| {
                candidates.iter().find(|units| {
                    units
                        .iter()
                        .all(|&unit| free(self, unit) || self.movable_to_software(unit))
                })
            })
            .cloned()
        else {
            return Ok(None);
        };
        for &unit in &units {
            let Some(address) = self.state.breakpoints[unit] else {
                continue;
            };
            let bus_address = own_pspr_address(self.core(), address)
                .expect("only breakpoints in the PSPR are moved");
            // Disabled breakpoints have no `DEBUG` instruction: they are placed when enabled.
            if self.state.breakpoints_enabled
                && let Err(error) = self.plant(bus_address)
            {
                tracing::warn!(
                    "TriCore core {}: the breakpoint at {address:#010x} keeps its trigger unit, \
                     no DEBUG instruction ({error})",
                    self.core()
                );
                return Ok(None);
            }
            self.state.breakpoints[unit] = None;
            self.write_trigger(unit)?;
        }
        Ok(Some(units))
    }

    /// Moves software breakpoints into trigger units that became free: the units compare the
    /// PC, so code that the program copies over them leaves them in place.
    fn promote_software(&mut self) -> Result<(), TriCoreError> {
        if self.state.software_first || self.memory.software.is_empty() {
            return Ok(());
        }
        for slot in 0..ocds::BREAKPOINT_SLOTS {
            let Some(address) = self.state.slots[slot] else {
                continue;
            };
            let Some(i) = self.software_at(address) else {
                continue;
            };
            let Some(unit) = (0..ocds::TRIGGER_UNITS).find(|&unit| !self.state.unit_used(unit))
            else {
                break;
            };
            // The unit first: the breakpoint stays in place throughout.
            self.state.breakpoints[unit] = Some(address);
            self.write_trigger(unit)?;
            let sw = self.memory.software.remove(i);
            self.unplant(&sw)?;
        }
        Ok(())
    }

    /// Plants a `DEBUG` instruction at bus address `address`, over the instruction there.
    fn plant(&mut self, address: u32) -> Result<(), TriCoreError> {
        let mut head = [0; 2];
        self.bus()
            .read_block(AddressMap::Target, address, &mut head)?;
        let mut original = vec![0; ocds::instruction_len(head[0])];
        self.bus()
            .read_block(AddressMap::Target, address, &mut original)?;
        let sw = SoftwareBreakpoint { address, original };
        let debug = sw.debug();
        if sw.original == debug {
            // Left by a session that did not end cleanly, or the program's own: the instruction
            // under it is unknown.
            return Err(TriCoreError::Protocol(
                "a DEBUG instruction is there already".to_string(),
            ));
        }

        self.write_csfr(ocds::SWEVT, ocds::SWEVT_HALT)?;
        self.bus().write_block(AddressMap::Target, address, debug)?;
        let mut check = vec![0; debug.len()];
        self.bus()
            .read_block(AddressMap::Target, address, &mut check)?;
        if check != debug {
            return Err(TriCoreError::Protocol(
                "the memory did not take the DEBUG instruction".to_string(),
            ));
        }
        self.memory.software.push(sw);
        Ok(())
    }

    /// Puts the instruction under a software breakpoint back, unless the code there changed by
    /// other means (the program was loaded again, for example).
    fn unplant(&mut self, sw: &SoftwareBreakpoint) -> Result<(), TriCoreError> {
        let mut code = vec![0; sw.original.len()];
        self.bus()
            .read_block(AddressMap::Target, sw.address, &mut code)?;
        if code == sw.debug() {
            self.bus()
                .write_block(AddressMap::Target, sw.address, &sw.original)?;
        }
        Ok(())
    }

    /// Clears the breakpoint of `slot`.
    fn clear_breakpoint(&mut self, slot: usize) -> Result<(), TriCoreError> {
        self.ensure_slot(slot)?;
        self.take_triggers()?;
        let Some(address) = self.state.slots[slot].take() else {
            // An empty slot: its trigger unit is disabled, which clears what another session may
            // have left there.
            if slot < ocds::TRIGGER_UNITS && !self.state.unit_used(slot) {
                self.write_trigger(slot)?;
            }
            return Ok(());
        };
        if self.state.slots.contains(&Some(address)) {
            // Another slot holds the same breakpoint.
            return Ok(());
        }
        if let Some(unit) = self.trigger_unit_of(address) {
            self.state.breakpoints[unit] = None;
            self.write_trigger(unit)?;
            self.promote_software()?;
        } else if let Some(i) = self.software_at(address) {
            let sw = self.memory.software.remove(i);
            self.unplant(&sw)?;
        }
        Ok(())
    }

    /// Enables or disables the breakpoints. Disabled software breakpoints are taken out of the
    /// code, and planted again when the breakpoints are enabled.
    fn set_breakpoints_enabled(&mut self, state: bool) -> Result<(), Error> {
        self.take_triggers()?;
        self.state.breakpoints_enabled = state;
        for unit in 0..ocds::TRIGGER_UNITS {
            if self.state.unit_used(unit) {
                self.write_trigger(unit)?;
            }
        }
        if state {
            for slot in 0..ocds::BREAKPOINT_SLOTS {
                if let Some(address) = self.state.slots[slot] {
                    self.place(slot, address)?;
                }
            }
        } else {
            for sw in std::mem::take(&mut self.memory.software) {
                self.unplant(&sw)?;
            }
        }
        Ok(())
    }

    /// Sets a data watchpoint on the `len` bytes at `address`: one trigger unit for a single
    /// byte, an even and odd pair (a range) otherwise. It takes the highest free units, leaving
    /// the low ones to breakpoints (a breakpoint slot prefers the unit of its own number).
    fn set_watchpoint_units(
        &mut self,
        address: u32,
        len: u32,
        access: WatchpointAccess,
    ) -> Result<(), Error> {
        if len == 0 {
            return Err(Error::Other("a data breakpoint needs a length".to_string()));
        }
        self.take_triggers()?;
        if !self.state.breakpoints_enabled {
            self.set_breakpoints_enabled(true)?;
        }
        self.clear_watchpoint_units(address)?;
        let candidates = if len == 1 {
            (0..ocds::TRIGGER_UNITS)
                .rev()
                .map(|unit| vec![unit])
                .collect::<Vec<_>>()
        } else {
            (0..ocds::TRIGGER_UNITS)
                .step_by(2)
                .rev()
                .map(|unit| vec![unit, unit + 1])
                .collect()
        };
        let Some(unit) = self.claim_units(&candidates)?.map(|units| units[0]) else {
            return Err(Error::Other(format!(
                "No free trigger unit for a data breakpoint on {len} byte(s) at {address:#010x}: \
                 the {} units are in use (one of more than one byte takes an even and odd pair)",
                ocds::TRIGGER_UNITS
            )));
        };
        let watchpoint = Watchpoint {
            address,
            len,
            access,
            unit,
        };
        self.state.watchpoints.push(watchpoint);
        for unit in (0..ocds::TRIGGER_UNITS).filter(|&u| watchpoint.uses(u)) {
            self.write_trigger(unit)?;
        }
        Ok(())
    }

    /// Clears the data watchpoint at `address`, if there is one.
    fn clear_watchpoint_units(&mut self, address: u32) -> Result<(), TriCoreError> {
        let Some(i) = self
            .state
            .watchpoints
            .iter()
            .position(|w| w.address == address)
        else {
            return Ok(());
        };
        self.take_triggers()?;
        let watchpoint = self.state.watchpoints.remove(i);
        for unit in (0..ocds::TRIGGER_UNITS).filter(|&u| watchpoint.uses(u)) {
            self.write_trigger(unit)?;
        }
        self.promote_software()
    }
}

impl Drop for TriCore<'_> {
    fn drop(&mut self) {
        // Give the software breakpoints back to the core state.
        self.state.software = std::mem::take(&mut self.memory.software);
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
        // The device can still be rebooting: a run that fails on an access is repeated (a run of
        // a running core does nothing).
        let start = Instant::now();
        loop {
            match self.run_core() {
                Err(TriCoreError::TransferFailed { .. }) if start.elapsed() < REBOOT_TIME => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                result => {
                    self.state.last_stm = None;
                    return Ok(result?);
                }
            }
        }
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

    /// The breakpoint slots: the trigger units, plus the software breakpoints that the PSPR of
    /// the core can hold.
    fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        Ok(ocds::BREAKPOINT_SLOTS as u32)
    }

    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        Ok(self
            .state
            .slots
            .iter()
            .map(|bp| bp.map(u64::from))
            .collect())
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), Error> {
        self.set_breakpoints_enabled(state)
    }

    fn set_hw_breakpoint(&mut self, unit_index: usize, addr: u64) -> Result<(), Error> {
        let address = u32::try_from(addr)
            .map_err(|_| Error::Other(format!("breakpoint address {addr:#x} out of range")))?;
        if !address.is_multiple_of(2) {
            return Err(Error::Other(format!(
                "breakpoint address {address:#010x} is not an instruction address"
            )));
        }
        self.set_breakpoint(unit_index, address)
    }

    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), Error> {
        Ok(self.clear_breakpoint(unit_index)?)
    }

    fn set_watchpoint(
        &mut self,
        address: u64,
        len: u64,
        access: WatchpointAccess,
    ) -> Result<(), Error> {
        let address = u32::try_from(address)
            .map_err(|_| Error::Other(format!("watchpoint address {address:#x} out of range")))?;
        let len = u32::try_from(len)
            .map_err(|_| Error::Other(format!("watchpoint length {len} out of range")))?;
        // The core accesses its local scratchpads through the local addresses; the triggers
        // compare the address as the core issues it, so it is used as given.
        self.set_watchpoint_units(address, len, access)
    }

    fn clear_watchpoint(&mut self, address: u64) -> Result<(), Error> {
        let address = u32::try_from(address)
            .map_err(|_| Error::Other(format!("watchpoint address {address:#x} out of range")))?;
        Ok(self.clear_watchpoint_units(address)?)
    }

    fn watchpoints(&mut self) -> Result<Vec<u64>, Error> {
        Ok(self
            .state
            .watchpoints
            .iter()
            .map(|w| w.address as u64)
            .collect())
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
