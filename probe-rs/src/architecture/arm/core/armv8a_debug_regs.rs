//! Debug register definitions for ARMv8-A
use bitfield::bitfield;
use std::mem::size_of;

use crate::{core::BreakpointCause, HaltReason};

/// A debug register that is accessible to the external debugger
pub trait Armv8DebugRegister {
    /// Register number
    const NUMBER: usize;

    /// The register's name.
    const NAME: &'static str;

    /// Get the address in the memory map
    fn get_mmio_address(base_address: u64) -> u64 {
        base_address + (Self::NUMBER * size_of::<u32>()) as u64
    }
}

bitfield! {
    /// EDSCR - Debug Status and Control Register
    #[derive(Copy, Clone)]
    pub struct Edscr(u32);
    impl Debug;

    /// Trace Filter Override. Overrides the Trace Filter controls allowing the external debugger to trace any visible Exception level.
    pub tfo, set_tfo: 31;

    /// DTRRX full.
    pub rxfull, set_rxfull: 30;

    /// DTRTX full.
    pub txfull, set_txfull: 29;

    /// ITR overrun.
    pub ito, _: 28;

    /// DTRRX overrun.
    pub rxo, _: 27;

    /// DTRTX underrun.
    pub txu, _: 26;

    /// Pipeline Advance. Indicates that software execution is progressing.
    pub pipeadv, _: 25;

    /// ITR empty.
    pub ite, set_ite: 24;

    /// Interrupt disable. Disables taking interrupts in Non-debug state.
    pub intdis, set_intdis: 23, 22;

    /// Traps accesses to the following debug System registers:
    ///
    /// AArch64: DBGBCR<n>_EL1, DBGBVR<n>_EL1, DBGWCR<n>_EL1, DBGWVR<n>_EL1.
    /// AArch32: DBGBCR<n>, DBGBVR<n>, DBGBXVR<n>, DBGWCR<n>, DBGWVR<n>.
    pub tda, set_tda: 21;

    /// Memory access mode. Controls the use of memory-access mode for accessing ITR and the DCC.
    pub ma, set_ma: 20;

    /// Sample CONTEXTIDR_EL2. Controls whether the PC Sample-based Profiling Extension samples CONTEXTIDR_EL2 or VTTBR_EL2.VMID.
    pub sc2, set_sc2: 19;

    /// Non-secure status. In Debug state, gives the current Security state
    pub ns, _: 18;

    /// Secure debug disabled.
    pub sdd, _: 16;

    /// Halting debug enable.
    pub hde, set_hde: 14;

    /// Exception level Execution state status.
    pub rw, set_rw: 13, 10;

    /// Exception level.
    pub el, _: 9, 8;

    /// SError interrupt pending.
    pub a, _: 7;

    /// Cumulative error flag.
    pub err, _: 6;

    /// Debug status flags.
    pub status, set_status: 5, 0;
}

impl Edscr {
    /// Is the core currently in a 64-bit mode?
    /// This is only accurate if inspected while halted
    pub fn currently_64_bit(&self) -> bool {
        // RW is a bitfield for each EL where bit n is ELn
        // If the bit is 1, that EL is Aarch64
        self.rw() & (1 << self.el()) > 0
    }

    /// Is the core halted?
    pub fn halted(&self) -> bool {
        match self.status() {
            // PE is restarting, exiting Debug state.
            0b000001 => false,
            // PE is in Non-debug state.
            0b000010 => false,
            // Breakpoint
            0b000111 => true,
            // External debug request.
            0b010011 => true,
            // Halting step, normal.
            0b011011 => true,
            // Halting step, exclusive.
            0b011111 => true,
            // OS Unlock catch.
            0b100011 => true,
            // Reset catch.
            0b100111 => true,
            // Watchpoint
            0b101011 => true,
            // HLT instruction.
            0b101111 => true,
            // Software access to debug register.
            0b110011 => true,
            // Exception Catch.
            0b110111 => true,
            // Halting step, no syndrome.
            0b111011 => true,
            // Everything else is running
            _ => false,
        }
    }

    /// Decode the MOE register into HaltReason
    pub fn halt_reason(&self) -> HaltReason {
        match self.status() {
            // Breakpoint debug event
            // TODO: The DBGDSCR register will contain information about whether this was a Software or Hardware breakpoint.
            0b000111 => HaltReason::Breakpoint(BreakpointCause::Unknown),
            // External debug request.
            0b010011 => HaltReason::Request,
            // Halting step
            0b011011 => HaltReason::Step,
            0b011111 => HaltReason::Step,
            0b111011 => HaltReason::Step,
            // OS Unlock catch.
            0b100011 => HaltReason::Exception,
            // Reset catch.
            0b100111 => HaltReason::Exception,
            // Watchpoint
            0b101011 => HaltReason::Watchpoint,
            // HLT instruction - causes entry into Debug state.
            0b101111 => HaltReason::Breakpoint(BreakpointCause::Software),
            // Software access to debug register.
            0b110011 => HaltReason::Exception,
            // Exception Catch.
            0b110111 => HaltReason::Exception,
            // All other values are reserved or running
            _ => HaltReason::Unknown,
        }
    }
}

impl Armv8DebugRegister for Edscr {
    const NUMBER: usize = 34;
    const NAME: &'static str = "EDSCR";
}

impl From<u32> for Edscr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Edscr> for u32 {
    fn from(value: Edscr) -> Self {
        value.0
    }
}

bitfield! {
    /// EDLAR - Lock Access Register
    #[derive(Copy, Clone)]
    pub struct Edlar(u32);
    impl Debug;

    /// Lock value
    pub value, set_value : 31, 0;

}

impl Armv8DebugRegister for Edlar {
    const NUMBER: usize = 1004;
    const NAME: &'static str = "EDLAR";
}

impl From<u32> for Edlar {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Edlar> for u32 {
    fn from(value: Edlar) -> Self {
        value.0
    }
}

bitfield! {
    /// DBGBVR - Breakpoint Value Register
    #[derive(Copy, Clone)]
    pub struct Dbgbvr(u32);
    impl Debug;

    /// Breakpoint address
    pub value, set_value : 31, 0;
}

impl Armv8DebugRegister for Dbgbvr {
    const NUMBER: usize = 256;
    const NAME: &'static str = "DBGBVR";
}

impl From<u32> for Dbgbvr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbgbvr> for u32 {
    fn from(value: Dbgbvr) -> Self {
        value.0
    }
}

bitfield! {
    /// DBGBCR - Breakpoint Control Register
    #[derive(Copy, Clone)]
    pub struct Dbgbcr(u32);
    impl Debug;

    /// Breakpoint type
    pub bt, set_bt : 23, 20;

    /// Linked breakpoint number
    pub lbn, set_lbn : 19, 16;

    /// Security state control
    pub ssc, set_ssc : 15, 14;

    /// Hyp mode control bit
    pub hmc, set_hmc: 13;

    /// Byte address select
    pub bas, set_bas: 8, 5;

    /// Privileged mode control
    pub pmc, set_pmc: 2, 1;

    /// Breakpoint enable
    pub e, set_e: 0;
}

impl Armv8DebugRegister for Dbgbcr {
    const NUMBER: usize = 258;
    const NAME: &'static str = "DBGBCR";
}

impl From<u32> for Dbgbcr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbgbcr> for u32 {
    fn from(value: Dbgbcr) -> Self {
        value.0
    }
}

bitfield! {
    /// EDDFR - External Debug Feature Register
    #[derive(Copy, Clone)]
    pub struct Eddfr(u32);
    impl Debug;

    /// Number of breakpoints that are context-aware, minus 1.
    pub ctx_cmps, _: 31, 28;

    /// Number of watchpoints, minus 1.
    pub wrps, _: 23, 20;

    /// Number of breakpoints, minus 1
    pub brps, set_brps: 15, 12;

    /// PMU Version
    pub pmuver, _: 11, 8;

    /// Trace Version
    pub tracever, _: 7, 4;
}

impl Armv8DebugRegister for Eddfr {
    const NUMBER: usize = 842;
    const NAME: &'static str = "EDDFR";
}

impl From<u32> for Eddfr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Eddfr> for u32 {
    fn from(value: Eddfr) -> Self {
        value.0
    }
}

bitfield! {
    /// EDITR - External Debug Instruction Transfer Register
    #[derive(Copy, Clone)]
    pub struct Editr(u32);
    impl Debug;

    /// Instruction value
    pub value, set_value: 31, 0;
}

impl Armv8DebugRegister for Editr {
    const NUMBER: usize = 33;
    const NAME: &'static str = "EDITR";
}

impl From<u32> for Editr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Editr> for u32 {
    fn from(value: Editr) -> Self {
        value.0
    }
}

bitfield! {
    /// EDRCR - External Debug Reserve Control Register
    #[derive(Copy, Clone)]
    pub struct Edrcr(u32);
    impl Debug;

    /// Allow imprecise entry to Debug state.
    pub cbrrq, set_cbrrq: 4;

    /// Clear Sticky Pipeline Advance.
    pub cpsa, set_cpsa: 3;

    /// Clear Sticky Error.
    pub cse, set_cse: 2;
}

impl Armv8DebugRegister for Edrcr {
    const NUMBER: usize = 36;
    const NAME: &'static str = "EDRCR";
}

impl From<u32> for Edrcr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Edrcr> for u32 {
    fn from(value: Edrcr) -> Self {
        value.0
    }
}

bitfield! {
    /// EDECR - External Debug Execution Control Register
    #[derive(Copy, Clone)]
    pub struct Edecr(u32);
    impl Debug;

    /// Halting step enable.
    pub ss, set_ss : 2;

    /// Reset Catch Enable.
    pub rce, set_rce : 1;

    /// OS Unlock Catch Enable.
    pub osuce, set_osuce : 0;
}

impl Armv8DebugRegister for Edecr {
    const NUMBER: usize = 9;
    const NAME: &'static str = "EDECR";
}

impl From<u32> for Edecr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Edecr> for u32 {
    fn from(value: Edecr) -> Self {
        value.0
    }
}

bitfield! {
    /// EDPRCR - External Debug Power/Reset Control Register
    #[derive(Copy, Clone)]
    pub struct Edprcr(u32);
    impl Debug;

    /// COREPURQ
    pub corepurq, set_corepurq : 3;

    /// Warm reset request.
    pub cwrr, set_cwrr : 1;

    /// Core no powerdown request.
    pub corenpdrq, set_corenpdrq : 0;
}

impl Armv8DebugRegister for Edprcr {
    const NUMBER: usize = 196;
    const NAME: &'static str = "EDPRCR";
}

impl From<u32> for Edprcr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Edprcr> for u32 {
    fn from(value: Edprcr) -> Self {
        value.0
    }
}

bitfield! {
    /// DBGDTRTX - Debug Data Transfer Register, Transmi
    #[derive(Copy, Clone)]
    pub struct Dbgdtrtx(u32);
    impl Debug;

    /// Instruction value
    pub value, set_value: 31, 0;
}

impl Armv8DebugRegister for Dbgdtrtx {
    const NUMBER: usize = 35;
    const NAME: &'static str = "DBGDTRTX";
}

impl From<u32> for Dbgdtrtx {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbgdtrtx> for u32 {
    fn from(value: Dbgdtrtx) -> Self {
        value.0
    }
}

bitfield! {
    /// DBGDTRRX - Debug Data Transfer Register, Receive
    #[derive(Copy, Clone)]
    pub struct Dbgdtrrx(u32);
    impl Debug;

    /// Instruction value
    pub value, set_value: 31, 0;
}

impl Armv8DebugRegister for Dbgdtrrx {
    const NUMBER: usize = 32;
    const NAME: &'static str = "DBGDTRRX";
}

impl From<u32> for Dbgdtrrx {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbgdtrrx> for u32 {
    fn from(value: Dbgdtrrx) -> Self {
        value.0
    }
}

bitfield! {
    /// EDPRSR - External Debug Processor Status Register
    #[derive(Copy, Clone)]
    pub struct Edprsr(u32);
    impl Debug;

    /// Sticky Debug Restart.
    pub sdr, set_sdr: 11;

    /// Sticky EPMAD error.
    pub spmad, _: 10;

    /// External Performance Monitors Non-secure Access Disable status.
    pub epmad, _: 9;

    /// Sticky EDAD error.
    pub sdad, _: 8;

    /// External Debug Access Disable status.
    pub edad, _: 7;

    /// Double Lock.
    pub dlk, _: 6;

    /// OS Lock status bit.
    pub oslk, _: 5;

    /// Halted status bit.
    pub halted, _: 4;

    /// Sticky core Reset status bit.
    pub sr, _: 3;

    /// PE Reset status bit.
    pub r, _: 2;

    /// Sticky core Powerdown status bit.
    pub spd, _: 1;

    /// Core powerup status bit.
    pub pu, _: 0;
}

impl Armv8DebugRegister for Edprsr {
    const NUMBER: usize = 197;
    const NAME: &'static str = "EDPRSR";
}

impl From<u32> for Edprsr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Edprsr> for u32 {
    fn from(value: Edprsr) -> Self {
        value.0
    }
}

bitfield! {
    /// CTICONTROL - CTI control register
    #[derive(Copy, Clone)]
    pub struct CtiControl(u32);
    impl Debug;

    /// Enables or disables the CTI mapping functions.
    pub glben, set_glben : 0;
}

impl Armv8DebugRegister for CtiControl {
    const NUMBER: usize = 0;
    const NAME: &'static str = "CTICONTROL";
}

impl From<u32> for CtiControl {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<CtiControl> for u32 {
    fn from(value: CtiControl) -> Self {
        value.0
    }
}

bitfield! {
    /// CTIGATE - CTI gate register
    #[derive(Copy, Clone)]
    pub struct CtiGate(u32);
    impl Debug;

    /// Enables or disables the CTI mapping functions.
    pub en, set_en : 0, 0, 32;
}

impl Armv8DebugRegister for CtiGate {
    const NUMBER: usize = 80;
    const NAME: &'static str = "CTIGATE";
}

impl From<u32> for CtiGate {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<CtiGate> for u32 {
    fn from(value: CtiGate) -> Self {
        value.0
    }
}

bitfield! {
    /// CTIOUTEN<n> - CTI output enable register
    #[derive(Copy, Clone)]
    pub struct CtiOuten(u32);
    impl Debug;

    /// Enables or disables input <n> generating this output
    pub outen, set_outen : 0, 0, 32;
}

impl Armv8DebugRegister for CtiOuten {
    const NUMBER: usize = 40;
    const NAME: &'static str = "CTIOUTEN";
}

impl From<u32> for CtiOuten {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<CtiOuten> for u32 {
    fn from(value: CtiOuten) -> Self {
        value.0
    }
}

bitfield! {
    /// CTIAPPPULSE - CTI application pulse register
    #[derive(Copy, Clone)]
    pub struct CtiApppulse(u32);
    impl Debug;

    /// Generate a pulse on channel N
    pub apppulse, set_apppulse : 0, 0, 32;
}

impl Armv8DebugRegister for CtiApppulse {
    const NUMBER: usize = 7;
    const NAME: &'static str = "CTIAPPPULSE";
}

impl From<u32> for CtiApppulse {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<CtiApppulse> for u32 {
    fn from(value: CtiApppulse) -> Self {
        value.0
    }
}

bitfield! {
    /// CTIINTACK - CTI Output Trigger Acknowledge register
    #[derive(Copy, Clone)]
    pub struct CtiIntack(u32);
    impl Debug;

    /// Ack trigger on channel N
    pub ack, set_ack : 0, 0, 32;
}

impl Armv8DebugRegister for CtiIntack {
    const NUMBER: usize = 4;
    const NAME: &'static str = "CTIINTACK";
}

impl From<u32> for CtiIntack {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<CtiIntack> for u32 {
    fn from(value: CtiIntack) -> Self {
        value.0
    }
}

bitfield! {
    /// CTITRIGOUTSTATUS - CTI Trigger Out Status register
    #[derive(Copy, Clone)]
    pub struct CtiTrigoutstatus(u32);
    impl Debug;

    /// Status on channel N
    pub status, _ : 0, 0, 32;
}

impl Armv8DebugRegister for CtiTrigoutstatus {
    const NUMBER: usize = 77;
    const NAME: &'static str = "CTITRIGOUTSTATUS";
}

impl From<u32> for CtiTrigoutstatus {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<CtiTrigoutstatus> for u32 {
    fn from(value: CtiTrigoutstatus) -> Self {
        value.0
    }
}
