//! Debug register definitions

use crate::{HaltReason, core::BreakpointCause, memory_mapped_bitfield_register};

memory_mapped_bitfield_register! {
    /// DBGDSCR - Debug Status and Control Registers
    pub struct Dbgdscr(u32);
    0x088, "DBGDSCR",
    impl From;

    /// DBGDTRRX register full. The possible values of this bit are:
    ///
    /// 0
    /// DBGDTRRX register empty.
    ///
    /// 1
    /// DBGDTRRX register full.
    pub rxfull, _: 30;

    /// DBGDTRTX register full. The possible values of this bit are:
    /// 0
    /// DBGDTRTX register empty.
    ///
    /// 1
    /// DBGDTRTX register full.
    pub txfull, _: 29;

    /// Latched RXfull. This controls the behavior of the processor on writes to DBGDTRRXext.
    pub rxfull_l, set_rxfull_l: 27;

    /// Latched TXfull. This controls the behavior of the processor on reads of DBGDTRTXext.
    pub txfull_l, set_txfull_l: 26;

    /// Sticky Pipeline Advance bit. This bit is set to 1 whenever the processor pipeline advances by retiring one or more instructions. It is cleared to 0 only by a write to DBGDRCR.CSPA.
    pub pipeadv, _: 25;

    /// Latched Instruction Complete. This is a copy of the internal InstrCompl flag, taken on each read of DBGDSCRext. InstrCompl signals whether the processor has completed execution of an instruction issued through DBGITR. InstrCompl is not visible directly in any register.
    ///
    /// On a read of DBGDSCRext when the processor is in Debug state, InstrCompl_l always returns the current value of InstrCompl. The meanings of the values of InstrCompl_l are:
    ///
    /// 0
    /// An instruction previously issued through the DBGITR has not completed its changes to the architectural state of the processor.
    ///
    /// 1
    /// All instructions previously issued through the DBGITR have completed their changes to the architectural state of the processor.
    pub instrcoml_l, set_instrcoml_l: 24;

    /// External DCC access mode. This field controls the access mode for the external views of the DCC registers and the DBGITR. Possible values are:
    ///
    /// 0b00
    /// Non-blocking mode.
    ///
    /// 0b01
    /// Stall mode.
    ///
    /// 0b10
    /// Fast mode.
    ///
    /// The value 0b11 is reserved.
    pub extdccmode, set_extdccmode: 21, 20;

    /// Asynchronous Aborts Discarded. The possible values of this bit are:
    ///
    /// 0
    /// Asynchronous aborts handled normally.
    ///
    /// 1
    /// On an asynchronous abort to which this bit applies, the processor sets the Sticky Asynchronous Abort bit, ADABORT_l, to 1 but otherwise discards the abort.
    pub adadiscard, _: 19;

    /// Non-secure state status. If the implementation includes the Security Extensions, this bit indicates whether the processor is in the Secure state. The possible values of this bit are:
    ///
    /// 0
    /// The processor is in the Secure state.
    ///
    /// 1
    /// The processor is in the Non-secure state.
    pub ns, _: 18;

    /// Secure PL1 Non-Invasive Debug Disabled. This bit shows if non-invasive debug is permitted in Secure PL1 modes. The possible values of the bit are:
    ///
    /// 0
    /// Non-invasive debug is permitted in Secure PL1 modes.
    ///
    /// 1
    /// Non-invasive debug is not permitted in Secure PL1 modes.
    pub spniddis, _: 17;

    /// Secure PL1 Invasive Debug Disabled bit. This bit shows if invasive debug is permitted in Secure PL1 modes. The possible values of the bit are:
    ///
    /// 0
    /// Invasive debug is permitted in Secure PL1 modes.
    ///
    /// 1
    /// Invasive debug is not permitted in Secure PL1 modes.
    pub spiddis, _: 16;

    /// Monitor debug-mode enable. The possible values of this bit are:
    ///
    /// 0
    /// Monitor debug-mode disabled.
    ///
    /// 1
    /// Monitor debug-mode enabled.
    pub mdbgen, set_mdbgen: 15;

    ///Halting debug-mode enable. The possible values of this bit are:
    ///
    /// 0
    /// Halting debug-mode disabled.
    ///
    /// 1
    /// Halting debug-mode enabled.
    pub hdbgen, set_hdbgen: 14;

    /// Execute ARM instruction enable. This bit enables the execution of ARM instructions through the DBGITR. The possible values of this bit are:
    ///
    /// 0
    /// ITR mechanism disabled.
    ///
    /// 1
    /// The ITR mechanism for forcing the processor to execute instructions in Debug state via the external debug interface is enabled.
    pub itren, set_itren: 13;

    /// User mode access to Debug Communications Channel (DCC) disable. The possible values of this bit are:
    ///
    /// 0
    /// User mode access to DCC enabled.
    ///
    /// 1
    /// User mode access to DCC disabled.
    pub udccdis, set_udccdis: 12;

    /// Interrupts Disable. Setting this bit to 1 masks the taking of IRQs and FIQs. The possible values of this bit are:
    ///
    /// 0
    /// Interrupts enabled.
    ///
    /// 1
    /// Interrupts disabled.
    pub intdis, set_intdis: 11;

    /// Force Debug Acknowledge. A debugger can use this bit to force any implemented debug acknowledge output signals to be asserted. The possible values of this bit are:
    ///
    /// 0
    /// Debug acknowledge signals under normal processor control.
    ///
    /// 1
    /// Debug acknowledge signals asserted, regardless of the processor state.
    pub dbgack, set_dbgack: 10;

    /// Fault status. This bit is updated on every Data Abort exception generated in Debug state, and might indicate that the exception syndrome information was written to the PL2 exception syndrome registers. The possible values are:
    ///
    /// 0
    /// Software must use the current state and mode and the value of HCR.TGE to determine which of the following sets of registers holds information about the Data Abort exception:
    ///
    /// The PL1 fault reporting registers, meaning the DFSR and DFAR, and the ADFSR if it is implemented.
    /// The PL2 fault syndrome registers, meaning the HSR, HDFAR, and HPFAR, and the HADFSR if it is implemented.
    /// 1
    /// Fault status information was written to the PL2 fault syndrome registers.
    pub fs, _: 9;

    /// Sticky Undefined Instruction. This bit is set to 1 by any Undefined Instruction exceptions generated by instructions issued to the processor while in Debug state. The possible values of this bit are:
    ///
    /// 0
    /// No Undefined Instruction exception has been generated since the last time this bit was cleared to 0.
    ///
    /// 1
    /// An Undefined Instruction exception has been generated since the last time this bit was cleared to 0.
    pub und_l, _: 8;

    /// Sticky Asynchronous Abort. When the ADAdiscard bit, bit[19], is set to 1, ADABORT_l is set to 1 by any asynchronous abort that occurs when the processor is in Debug state.
    ///
    /// The possible values of this bit are:
    ///
    /// 0
    /// No asynchronous abort has been generated since the last time this bit was cleared to 0.
    ///
    /// 1
    /// Since the last time this bit was cleared to 0, an asynchronous abort has been generated while ADAdiscard was set to 1.
    pub adabort_l, _e: 7;

    /// Sticky Synchronous Data Abort. This bit is set to 1 by any Data Abort exception that is generated synchronously when the processor is in Debug state. The possible values of this bit are:
    ///
    /// 0
    /// No synchronous Data Abort exception has been generated since the last time this bit was cleared to 0.
    ///
    /// 1
    /// A synchronous Data Abort exception has been generated since the last time this bit was cleared to 0.
    pub sdabort_l, _: 6;

    /// Method of Debug entry.
    pub moe, _: 5, 2;

    /// Processor Restarted. The possible values of this bit are:
    ///
    /// 0
    /// The processor is exiting Debug state. This bit only reads as 0 between receiving a restart request, and restarting Non-debug state operation.
    ///
    /// 1
    /// The processor has exited Debug state. This bit remains set to 1 if the processor re-enters Debug state.
    pub restarted, set_restarted: 1;

    /// Processor Halted. The possible values of this bit are:
    ///
    /// 0
    /// The processor is in Non-debug state.
    ///
    /// 1
    /// The processor is in Debug state.
    pub halted, set_halted: 0;
}

impl Dbgdscr {
    /// Decode the MOE register into HaltReason
    pub fn halt_reason(&self) -> HaltReason {
        if self.halted() {
            match self.moe() {
                // Halt request from debugger
                0b0000 => HaltReason::Request,
                // Breakpoint debug event
                0b0001 => HaltReason::Breakpoint(BreakpointCause::Hardware),
                // Async watchpoint debug event
                0b0010 => HaltReason::Watchpoint,
                // BKPT instruction
                0b0011 => HaltReason::Breakpoint(BreakpointCause::Software),
                // External halt request
                0b0100 => HaltReason::External,
                // Vector catch
                0b0101 => HaltReason::Exception,
                // OS Unlock vector catch
                0b1000 => HaltReason::Exception,
                // Sync watchpoint debug event
                0b1010 => HaltReason::Watchpoint,
                // All other values are reserved
                _ => HaltReason::Unknown,
            }
        } else {
            // Not halted or cannot detect
            HaltReason::Unknown
        }
    }
}

memory_mapped_bitfield_register! {
    /// DBGDIDR - Debug ID Register
    pub struct Dbgdidr(u32);
    0x000, "DBGDIDR",
    impl From;

    /// The number of watchpoints implemented. The number of implemented watchpoints is one more than the value of this field.
    pub wrps, _: 31, 28;

    /// The number of breakpoints implemented. The number of implemented breakpoints is one more than value of this field.
    pub brps, set_brps: 27, 24;

    /// The number of breakpoints that can be used for Context matching. This is one more than the value of this field.
    pub ctx_cmps, _: 23, 20;

    /// The Debug architecture version. The permitted values of this field are:
    ///
    /// 0b0001
    /// ARMv6, v6 Debug architecture.
    ///
    /// 0b0010
    /// ARMv6, v6.1 Debug architecture.
    ///
    /// 0b0011
    /// ARMv7, v7 Debug architecture, with all CP14 registers implemented.
    ///
    /// 0b0100
    /// ARMv7, v7 Debug architecture, with only the baseline CP14 registers implemented.
    ///
    /// 0b0101
    /// ARMv7, v7.1 Debug architecture.
    ///
    /// All other values are reserved.
    pub version, _: 19, 16;

    /// Debug Device ID Register, DBGDEVID, implemented.
    pub devid_imp, _: 15;

    /// Secure User halting debug not implemented
    pub nsuhd_imp, _: 14;

    /// Program Counter Sampling Register, DBGPCSR, implemented as register 33.
    pub pcsr_imp, _: 13;

    /// Security Extensions implemented.
    pub se_imp, _: 12;

    /// This field holds an implementation defined variant number.
    pub variant, _: 7, 4;

    /// This field holds an implementation defined revision number.
    pub revision, _: 3, 0;
}

memory_mapped_bitfield_register! {
    /// DBGDRCR - Debug Run Control Register
    pub struct Dbgdrcr(u32);
    0x090, "DBGDRCR",
    impl From;

    /// Cancel Bus Requests Request
    pub cbrrq, set_cbrrq: 4;

    /// Clear Sticky Pipeline Advance
    pub cspa, set_cspa: 3;

    /// Clear Sticky Exceptions
    pub cse, set_cse: 2;

    /// Restart request
    pub rrq, set_rrq: 1;

    /// Halt request
    pub hrq, set_hrq: 0;
}

memory_mapped_bitfield_register! {
    /// DBGBVR - Breakpoint Value Register
    pub struct Dbgbvr(u32);
    0x100, "DBGBVR",
    impl From;

    /// Breakpoint address
    pub value, set_value : 31, 0;
}

memory_mapped_bitfield_register! {
    /// DBGBCR - Breakpoint Control Register
    pub struct Dbgbcr(u32);
    0x140, "DBGBCR",
    impl From;

    /// Address range mask. Whether masking is supported is implementation defined.
    pub mask, set_mask : 28, 24;

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
    /// DBGLAR - Lock Access Register
    pub struct Dbglar(u32);
    0xFB0, "DBGLAR",
    impl From;

    /// Lock value
    pub value, set_value : 31, 0;

}

memory_mapped_bitfield_register! {
    /// DBGDSCCR - State Cache Control Register
    pub struct Dbgdsccr(u32);
    0x028, "DBGDSCCR",
    impl From;

    /// Force Write-Through
    pub nwt, set_nwt: 2;

    /// Instruction cache
    pub nil, set_nil: 1;

    /// Data or unified cache.
    pub ndl, set_ndl: 0;
}

memory_mapped_bitfield_register! {
    /// DBGDSMCR - Debug State MMU Control Register
    pub struct Dbgdsmcr(u32);
    0x02C, "DBGDSMCR",
    impl From;

    /// Instruction TLB matching bit
    pub nium, set_nium: 3;

    /// Data or Unified TLB matching bit
    pub ndum, set_ndum: 2;

    /// Instruction TLB loading bit
    pub niul, set_niul: 1;

    /// Data or Unified TLB loading bit
    pub ndul, set_ndul: 0;
}

memory_mapped_bitfield_register! {
    /// DBGITR - Instruction Transfer Register
    pub struct Dbgitr(u32);
    0x084, "DBGITR",
    impl From;

    /// Instruction value
    pub value, set_value: 31, 0;
}

memory_mapped_bitfield_register! {
    /// DBGDTRTX - Target to Host data transfer register
    pub struct Dbgdtrtx(u32);
    0x08C, "DBGDTRTX",
    impl From;

    /// Value
    pub value, set_value: 31, 0;
}

memory_mapped_bitfield_register! {
    /// DBGDTRRX - Host to Target data transfer register
    pub struct Dbgdtrrx(u32);
    0x080, "DBGDTRRX",
    impl From;

    /// Value
    pub value, set_value: 31, 0;
}

memory_mapped_bitfield_register! {
    /// DBGPRCR - Powerdown and Reset Control Register
    pub struct Dbgprcr(u32);
    0x310, "DBGPRCR",
    impl From;

    /// Core powerup request
    pub corepurq, set_corepurq : 3;

    /// Hold core in warm reset
    pub hcwr, set_hcwr : 2;

    /// Core warm reset request
    pub cwrr, set_cwrr : 1;

    /// Core no powerdown request
    pub corenpdrq, set_corenpdrq : 0;
}

memory_mapped_bitfield_register! {
    /// DBGPRSR - Powerdown and Reset Status Register
    pub struct Dbgprsr(u32);
    0x314, "DBGPRSR",
    impl From;

    /// OS Double Lock Status
    pub dlk, _ : 6;

    /// OS Lock Status
    pub oslk, _ : 5;

    /// Halted
    pub halted, _ : 4;

    /// Stick reset status
    pub sr, _ : 3;

    /// Reset status
    pub r, _ : 2;

    /// Stick power down status
    pub spd, _ : 1;

    /// Power up status
    pub pu, _ : 0;
}
