//! Debug register definitions for ARMv8-A

use crate::{HaltReason, core::BreakpointCause, memory_mapped_bitfield_register};

memory_mapped_bitfield_register! {
    /// EDSCR - Debug Status and Control Register
    pub struct Edscr(u32);
    0x088, "EDSCR",
    impl From;

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

memory_mapped_bitfield_register! {
    /// EDLAR - Lock Access Register
    pub struct Edlar(u32);
    0xFB0,"EDLAR",
    impl From;

    /// Lock value
    pub value, set_value : 31, 0;

}

memory_mapped_bitfield_register! {
    /// OSLAR_EL1 - OS Lock Access Register
    pub struct Oslar(u32);
    0x300,"OSLAR_EL1",
    impl From;

    /// Lock value
    pub oslk, set_oslk: 1, 0;
}

memory_mapped_bitfield_register! {
    /// DBGBVR - Breakpoint Value Register
    pub struct Dbgbvr(u32);
    0x400, "DBGBVR",
    impl From;

    /// Breakpoint address
    pub value, set_value : 31, 0;
}

memory_mapped_bitfield_register! {
    /// DBGBCR - Breakpoint Control Register
    pub struct Dbgbcr(u32);
    0x408, "DBGBCR",
    impl From;

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

memory_mapped_bitfield_register! {
    /// EDDFR - External Debug Feature Register
    pub struct Eddfr(u32);
    0xD28, "EDDFR",
    impl From;

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

memory_mapped_bitfield_register! {
    /// EDITR - External Debug Instruction Transfer Register
    pub struct Editr(u32);
    0x084, "EDITR",
    impl From;

    /// Instruction value
    pub value, set_value: 31, 0;
}

memory_mapped_bitfield_register! {
    /// EDRCR - External Debug Reserve Control Register
    pub struct Edrcr(u32);
    0x090, "EDRCR",
    impl From;

    /// Allow imprecise entry to Debug state.
    pub cbrrq, set_cbrrq: 4;

    /// Clear Sticky Pipeline Advance.
    pub cpsa, set_cpsa: 3;

    /// Clear Sticky Error.
    pub cse, set_cse: 2;
}

memory_mapped_bitfield_register! {
    /// EDECR - External Debug Execution Control Register
    pub struct Edecr(u32);
    0x024, "EDECR",
    impl From;

    /// Halting step enable.
    pub ss, set_ss : 2;

    /// Reset Catch Enable.
    pub rce, set_rce : 1;

    /// OS Unlock Catch Enable.
    pub osuce, set_osuce : 0;
}

memory_mapped_bitfield_register! {
    /// EDPRCR - External Debug Power/Reset Control Register
    pub struct Edprcr(u32);
    0x310, "EDPRCR",
    impl From;

    /// COREPURQ
    pub corepurq, set_corepurq : 3;

    /// Warm reset request.
    pub cwrr, set_cwrr : 1;

    /// Core no powerdown request.
    pub corenpdrq, set_corenpdrq : 0;
}

memory_mapped_bitfield_register! {
    /// DBGDTRTX - Debug Data Transfer Register, Transmit
    pub struct Dbgdtrtx(u32);
    0x08C, "DBGDTRTX",
    impl From;

    /// Instruction value
    pub value, set_value: 31, 0;
}

memory_mapped_bitfield_register! {
    /// DBGDTRRX - Debug Data Transfer Register, Receive
    pub struct Dbgdtrrx(u32);
    0x080, "DBGDTRRX",
    impl From;

    /// Instruction value
    pub value, set_value: 31, 0;
}

memory_mapped_bitfield_register! {
    /// EDPRSR - External Debug Processor Status Register
    pub struct Edprsr(u32);
    0x314, "EDPRSR",
    impl From;

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

memory_mapped_bitfield_register! {
    /// CTICONTROL - CTI control register
    pub struct CtiControl(u32);
    0x000, "CTICONTROL",
    impl From;

    /// Enables or disables the CTI mapping functions.
    pub glben, set_glben : 0;
}

memory_mapped_bitfield_register! {
    /// CTIGATE - CTI gate register
    pub struct CtiGate(u32);
    0x140, "CTIGATE",
    impl From;

    /// Enables or disables the CTI mapping functions.
    pub en, set_en : 0, 0, 32;
}

memory_mapped_bitfield_register! {
    /// CTIOUTEN<n> - CTI output enable register
    pub struct CtiOuten(u32);
    0x0A0, "CTIOUTEN",
    impl From;

    /// Enables or disables input <n> generating this output
    pub outen, set_outen : 0, 0, 32;
}

memory_mapped_bitfield_register! {
    /// CTIAPPPULSE - CTI application pulse register
    pub struct CtiApppulse(u32);
    0x01C, "CTIAPPPULSE",
    impl From;

    /// Generate a pulse on channel N
    pub apppulse, set_apppulse : 0, 0, 32;
}

memory_mapped_bitfield_register! {
    /// CTIINTACK - CTI Output Trigger Acknowledge register
    pub struct CtiIntack(u32);
    0x010, "CTIINTACK",
    impl From;

    /// Ack trigger on channel N
    pub ack, set_ack : 0, 0, 32;
}

memory_mapped_bitfield_register! {
    /// CTITRIGOUTSTATUS - CTI Trigger Out Status register
    pub struct CtiTrigoutstatus(u32);
    0x134, "CTITRIGOUTSTATUS",
    impl From;

    /// Status on channel N
    pub status, _ : 0, 0, 32;
}
