//! Register types and the core interface for armv7-M

use super::{
    cortex_m::Mvfr0,
    registers::cortex_m::{
        CORTEX_M_CORE_REGISTERS, CORTEX_M_WITH_FP_CORE_REGISTERS, FP, PC, RA, SP,
    },
    CortexMState, Dfsr,
};
use crate::{
    architecture::arm::{
        core::registers::cortex_m::XPSR, memory::ArmMemoryInterface, sequences::ArmDebugSequence,
        ArmError,
    },
    core::{
        Architecture, CoreInformation, CoreInterface, CoreRegisters, CoreStatus, HaltReason,
        MemoryMappedRegister, RegisterId, RegisterValue, VectorCatchCondition,
    },
    error::Error,
    memory::valid_32bit_address,
    BreakpointCause, CoreRegister, CoreType, InstructionSet,
};
use bitfield::bitfield;
use std::{
    mem::size_of,
    sync::Arc,
    time::{Duration, Instant},
};

bitfield! {
    /// Debug Halting Control and Status Register, DHCSR (see armv7-M Architecture Reference Manual C1.6.2)
    ///
    /// To write this register successfully, you need to set the debug key via [`Dhcsr::enable_write`] first!
    #[derive(Copy, Clone)]
    pub struct Dhcsr(u32);
    impl Debug;
    /// Indicates whether the processor has been reset since the last read of DHCSR:
    /// `0`: No reset since last DHCSR read.\
    /// `1`: At least one reset since last DHCSR read.
    ///
    /// This is a sticky bit, that clears to `0` on a read of DHCSR.
    pub s_reset_st, _: 25;
    /// When not in Debug state, indicates whether the processor has completed
    /// the execution of an instruction since the last read of DHCSR:
    ///
    /// `0`: No instruction has completed since last DHCSR read.\
    /// `1`: At least one instructions has completed since last DHCSR read.
    ///
    /// This is a sticky bit, that clears to `0` on a read of DHCSR.
    ///
    /// This bit is UNKNOWN:
    ///
    /// - after a Local reset, but is set to `1` as soon as the processor completes
    /// execution of an instruction.
    /// - when S_LOCKUP is set to 1.
    /// - when S_HALT is set to 1.
    ///
    /// When the processor is not in Debug state, a debugger can check this bit to
    /// determine if the processor is stalled on a load, store or fetch access.
    pub s_retire_st, _: 24;
    ///  Indicates whether the processor is locked up because of an unrecoverable
    /// exception:
    ///
    /// `0`: Not locked up.\
    /// `1`: Locked up.
    ///
    /// See Unrecoverable exception cases on page B1-206 for more information.
    ///
    /// This bit can only read as `1` when accessed by a remote debugger using the
    /// DAP. The value of `1` indicates that the processor is running but locked up.
    ///
    /// The bit clears to `0` when the processor enters Debug state.
    pub s_lockup, _: 19;
    /// Indicates whether the processor is sleeping:
    ///
    /// `0`: Not sleeping.\
    /// `1`: Sleeping.
    ///
    /// The debugger must set the DHCSR.C_HALT bit to `1` to gain control, or
    /// wait for an interrupt or other wakeup event to wakeup the system
    pub s_sleep, _: 18;
    /// Indicates whether the processor is in Debug state:
    ///
    /// `0`: Not in Debug state.\
    /// `1`: In Debug state.
    pub s_halt, _: 17;
    /// A handshake flag for transfers through the DCRDR:
    ///
    /// - Writing to DCRSR clears the bit to 0.
    /// - Completion of the DCRDR transfer then sets the bit to 1.
    ///
    /// For more information about DCRDR transfers see Debug Core Register
    /// Data Register, DCRDR on page C1-292.
    ///
    /// `0`: There has been a write to the DCRDR, but the transfer is not complete.\
    /// `1`: The transfer to or from the DCRDR is complete.
    ///
    /// This bit is only valid when the processor is in Debug state, otherwise the
    /// bit is UNKNOWN.
    pub s_regrdy, _: 16;
    /// Allow imprecise entry to Debug state. The actions on writing to this bit are:
    ///
    /// `0`: No action.\
    /// `1`: Allow imprecise entry to Debug state, for example by forcing any stalled load
    /// or store instruction to complete.
    ///
    /// Setting this bit to `1` allows a debugger to request imprecise entry to Debug state.
    ///
    /// The effect of setting this bit to `1` is UNPREDICTABLE unless the DHCSR write also sets
    /// C_DEBUGEN and C_HALT to 1. This means that if the processor is not already in Debug
    /// state it enters Debug state when the stalled instruction completes.
    ///
    /// Writing `1` to this bit makes the state of the memory system UNPREDICTABLE. Therefore, if a
    /// debugger writes `1` to this bit it must reset the processor before leaving Debug state.
    ///
    /// **Note**
    ///
    /// - A debugger can write to the DHCSR to clear this bit to 0. However, this does not
    /// remove the UNPREDICTABLE state of the memory system caused by setting C_SNAPSTALL to 1.
    /// - The architecture does not guarantee that setting this bit to `1` will force entry to Debug state.
    /// - Arm strongly recommends that a value of `1` is never written to C_SNAPSTALL when
    /// the processor is in Debug state.
    ///
    /// A power-on reset sets this bit to `0`.
    pub c_snapstall, set_c_snapstall: 5;
    /// When debug is enabled, the debugger can write to this bit to mask
    /// PendSV, SysTick and external configurable interrupts:
    ///
    /// `0`: Do not mask.\
    /// `1`: Mask PendSV, SysTick and external configurable interrupts.
    ///
    /// The effect of any attempt to change the value of this bit is UNPREDICTABLE
    /// unless both:
    ///
    /// - before the write to DHCSR, the value of the C_HALT bit is `1`.
    /// - the write to the DHCSR that changes the C_MASKINTS bit also
    /// writes `1` to the C_HALT bit.
    ///
    /// This means that a single write to DHCSR cannot set the C_HALT to `0` and
    /// change the value of the C_MASKINTS bit.
    ///
    /// The bit does not affect NMI. When DHCSR.C_DEBUGEN is set to `0`, the
    /// value of this bit is UNKNOWN.
    ///
    /// For more information about the use of this bit see Table C1-9 on
    /// page C1-282.
    ///
    /// This bit is UNKNOWN after a power-on reset.
    pub c_maskints, set_c_maskints: 3;
    /// Processor step bit. The effects of writes to this bit are:
    ///
    /// `0`: Single-stepping disabled.\
    /// `1`: Single-stepping enabled.
    ///
    /// For more information about the use of this bit see Table C1-9 on
    /// page C1-282.
    ///
    /// This bit is UNKNOWN after a power-on reset.
    pub c_step, set_c_step: 2;
    /// Processor halt bit. The effects of writes to this bit are:
    ///
    /// `0`: Request a halted processor to run.\
    /// `1`: Request a running processor to halt.
    ///
    /// Table C1-9 on page C1-282 shows the effect of writes to this bit when the
    /// processor is in Debug state.
    ///
    /// This bit is `0` after a System reset
    pub c_halt, set_c_halt: 1;
    ///  Halting debug enable bit:
    ///
    /// `0`: Halting debug disabled.\
    /// `1`: Halting debug enabled.
    ///
    /// If a debugger writes to DHCSR to change the value of this bit from `0` to
    /// `1`, it must also write `0` to the C_MASKINTS bit, otherwise behavior is UNPREDICTABLE.
    ///
    /// This bit can only be written from the DAP. Access to the DHCSR from
    /// software running on the processor is IMPLEMENTATION DEFINED.
    ///
    /// However, writes to this bit from software running on the processor are ignored.
    ///
    /// This bit is `0` after a power-on reset.
    pub c_debugen, set_c_debugen: 0;
}

impl Dhcsr {
    /// This function sets the bit to enable writes to this register.
    pub fn enable_write(&mut self) {
        self.0 &= !(0xffff << 16);
        self.0 |= 0xa05f << 16;
    }
}

impl From<u32> for Dhcsr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dhcsr> for u32 {
    fn from(value: Dhcsr) -> Self {
        value.0
    }
}

impl MemoryMappedRegister<u32> for Dhcsr {
    const ADDRESS_OFFSET: u64 = 0xE000_EDF0;
    const NAME: &'static str = "DHCSR";
}

/// Debug Core Register Data Register, DCRDR (see armv7-M Architecture Reference Manual C1.6.3)
#[derive(Debug, Copy, Clone)]
pub struct Dcrdr(u32);

impl From<u32> for Dcrdr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dcrdr> for u32 {
    fn from(value: Dcrdr) -> Self {
        value.0
    }
}

impl MemoryMappedRegister<u32> for Dcrdr {
    const ADDRESS_OFFSET: u64 = 0xE000_EDF8;
    const NAME: &'static str = "DCRDR";
}

bitfield! {
    /// Application Interrupt and Reset Control Register, AIRCR (see armv7-M Architecture Reference Manual B3.2.6)
    ///
    /// [`Aircr::vectkey`] must be called before this register can effectively be written!
    #[derive(Copy, Clone)]
    pub struct Aircr(u32);
    impl Debug;
    /// Vector Key. The value 0x05FA must be written to this register, otherwise
    /// the register write is UNPREDICTABLE.
    get_vectkeystat, set_vectkey: 31,16;
    /// Indicates the memory system data endianness:
    ///
    /// `0`: little endian.\
    /// `1`: big endian.
    ///
    /// See Endian support on page A3-44 for more information.
    pub endianness, set_endianness: 15;
    /// Priority grouping, indicates the binary point position.
    ///
    /// For information about the use of this field see Priority grouping on page B1-527.
    ///
    /// This field resets to `0b000`.
    pub prigroup, set_prigroup: 10,8;
    /// System Reset Request:
    ///
    /// `0`: do not request a reset.\
    /// `1`: request reset.
    ///
    /// Writing `1` to this bit asserts a signal to request a reset by the external
    /// system. The system components that are reset by this request are
    /// IMPLEMENTATION DEFINED. A Local reset is required as part of a system
    /// reset request.
    ///
    /// A Local reset clears this bit to `0`.
    ///
    /// See Reset management on page B1-208 for more information
    pub sysresetreq, set_sysresetreq: 2;
    /// Clears all active state information for fixed and configurable exceptions:
    ///
    /// `0`: do not clear state information.\
    /// `1`: clear state information.
    ///
    /// The effect of writing a `1` to this bit if the processor is not halted in Debug
    /// state is UNPREDICTABLE.
    pub vectclractive, set_vectclractive: 1;
    /// Writing `1` to this bit causes a local system reset, see Reset management on page B1-559 for
    /// more information. This bit self-clears.
    ///
    /// The effect of writing a `1` to this bit if the processor is not halted in Debug state is UNPREDICTABLE.
    ///
    /// When the processor is halted in Debug state, if a write to the register writes a `1` to both
    /// VECTRESET and SYSRESETREQ, the behavior is UNPREDICTABLE.
    ///
    /// This bit is write only.
    pub vectreset, set_vectreset: 0;
}

impl From<u32> for Aircr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Aircr> for u32 {
    fn from(value: Aircr) -> Self {
        value.0
    }
}

impl Aircr {
    /// Must be called before writing the register.
    pub fn vectkey(&mut self) {
        self.set_vectkey(0x05FA);
    }

    /// Verifies that the vector key is correct (see [`Aircr::vectkey`])
    pub fn vectkeystat(&self) -> bool {
        self.get_vectkeystat() == 0xFA05
    }
}

impl MemoryMappedRegister<u32> for Aircr {
    const ADDRESS_OFFSET: u64 = 0xE000_ED0C;
    const NAME: &'static str = "AIRCR";
}

bitfield! {
    /// Debug Exception and Monitor Control Register, DEMCR (see armv7-M Architecture Reference Manual C1.6.5)
    #[derive(Copy, Clone)]
    pub struct Demcr(u32);
    impl Debug;
    /// Global enable for DWT and ITM features
    pub trcena, set_trcena: 24;
    /// DebugMonitor semaphore bit
    pub mon_req, set_mon_req: 19;
    /// Step the processor?
    pub mon_step, set_mon_step: 18;
    /// Sets or clears the pending state of the DebugMonitor exception
    pub mon_pend, set_mon_pend: 17;
    /// Enable the DebugMonitor exception
    pub mon_en, set_mon_en: 16;
    /// Enable halting debug trap on a HardFault exception
    pub vc_harderr, set_vc_harderr: 10;
    /// Enable halting debug trap on a fault occurring during exception entry
    /// or exception return
    pub vc_interr, set_vc_interr: 9;
    /// Enable halting debug trap on a BusFault exception
    pub vc_buserr, set_vc_buserr: 8;
    /// Enable halting debug trap on a UsageFault exception caused by a state
    /// information error, for example an Undefined Instruction exception
    pub vc_staterr, set_vc_staterr: 7;
    /// Enable halting debug trap on a UsageFault exception caused by a
    /// checking error, for example an alignment check error
    pub vc_chkerr, set_vc_chkerr: 6;
    /// Enable halting debug trap on a UsageFault caused by an access to a
    /// Coprocessor
    pub vc_nocperr, set_vc_nocperr: 5;
    /// Enable halting debug trap on a MemManage exception.
    pub vc_mmerr, set_vc_mmerr: 4;
    /// Enable Reset Vector Catch
    pub vc_corereset, set_vc_corereset: 0;
}

impl From<u32> for Demcr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Demcr> for u32 {
    fn from(value: Demcr) -> Self {
        value.0
    }
}

impl MemoryMappedRegister<u32> for Demcr {
    const ADDRESS_OFFSET: u64 = 0xe000_edfc;
    const NAME: &'static str = "DEMCR";
}

bitfield! {
    /// Flash Patch Control Register, FP_CTRL (see armv7-M Architecture Reference Manual C1.11.3)
    #[derive(Copy,Clone)]
    pub struct FpCtrl(u32);
    impl Debug;
    /// Flash Patch breakpoint architecture revision:
    ///
    /// `0b0000` Flash Patch breakpoint version 1.\
    /// `0b0001` Flash Patch breakpoint version 2. Supports breakpoints on any location in the 4GB address range.
    pub rev, _: 31, 28;
    num_code_1, _: 14, 12;
    /// The number of literal address comparators supported, starting from NUM_CODE upwards.
    /// UNK/SBZP if Flash Patch is not implemented. Flash Patch is not implemented if `FP_REMAP[29]` is `0`.
    ///
    /// If this field is zero, the implementation does not support literal comparators.
    pub num_lit, _: 11, 8;
    num_code_0, _: 7, 4;
    /// On any write to FP_CTRL, this bit must be `1`. A write to the register with this bit set to zero
    /// is ignored. The Flash Patch Breakpoint unit ignores the write unless this bit is `1`.
    pub _, set_key: 1;
    /// Enable bit for the FPB:
    ///
    /// `0`: Flash Patch breakpoint disabled.\
    /// `1`: Flash Patch breakpoint enabled.
    ///
    /// A power-on reset clears this bit to `0`.
    pub enable, set_enable: 0;
}

impl FpCtrl {
    /// The number of instruction address comparators.
    /// If NUM_CODE is zero, the implementation does not support any instruction address comparators.
    pub fn num_code(&self) -> u32 {
        (self.num_code_1() << 4) | self.num_code_0()
    }
}

impl MemoryMappedRegister<u32> for FpCtrl {
    const ADDRESS_OFFSET: u64 = 0xE000_2000;
    const NAME: &'static str = "FP_CTRL";
}

impl From<u32> for FpCtrl {
    fn from(value: u32) -> Self {
        FpCtrl(value)
    }
}

impl From<FpCtrl> for u32 {
    fn from(value: FpCtrl) -> Self {
        value.0
    }
}

bitfield! {
    /// Flash Patch Comparator register, FP_COMPn (see armv7-M Architecture Reference Manual C1.11.5)
    #[derive(Copy,Clone)]
    pub struct FpRev1CompX(u32);
    impl Debug;
    /// For an instruction address comparator:
    ///
    /// Defines the behavior when the COMP address is matched:
    ///
    /// `00` Remap to remap address, see Flash Patch Remap register,
    /// FP_REMAP on page C1-758.
    ///
    /// When the comparators are enabled in the FP_CTRL register, if the
    /// implementation does not support remapping, the effect of an
    /// instruction address match with an enabled comparator with
    /// REPLACE programmed to 0b00 is UNPREDICTABLE.
    ///
    /// `01`: Breakpoint on instruction at `'000':COMP:'00'`.\
    /// `10`: Breakpoint on instruction at `'000':COMP:'10'`.\
    /// `11`: Breakpoint on both instructions at `'000':COMP:'00'` and `'000':COMP:'10'`.
    ///
    /// The reset value of this field is UNKNOWN.
    ///
    /// For a literal address comparator:
    ///
    /// Field is UNK/SBZP
    pub replace, set_replace: 31, 30;
    /// Bits[28:2] of the address to compare with addresses from the Code memory region,
    /// see The system address map on page B3-592. Bits[31:29] of the address for comparison are zero.
    ///
    /// For a literal address or instruction address remap, bits[1:0] of the comparison are also zero.
    ///
    /// For an instruction address breakpoint, bits[1:0] of the comparison are encoded by the REPLACE field.
    ///
    /// If a match occurs:
    ///
    /// - For an instruction address comparator, the REPLACE field defines the required action.
    /// - For a literal address comparator, the FPB remaps the access, see Flash Patch Remap register, FP_REMAP on page C1-758.
    ///
    /// The reset value of this field is UNKNOWN.
    pub comp, set_comp: 28, 2;
    /// Enable bit for this comparator:
    ///
    /// `0`: Comparator disabled.\
    /// `1`: Comparator enabled.
    ///
    /// A power-on reset clears this bit to `0`.
    pub enable, set_enable: 0;
}

impl MemoryMappedRegister<u32> for FpRev1CompX {
    const ADDRESS_OFFSET: u64 = 0xE000_2008;
    const NAME: &'static str = "FP_CTRL";
}

impl From<u32> for FpRev1CompX {
    fn from(value: u32) -> Self {
        FpRev1CompX(value)
    }
}

impl From<FpRev1CompX> for u32 {
    fn from(value: FpRev1CompX) -> Self {
        value.0
    }
}

impl FpRev1CompX {
    /// Get the correct comparator value stored at the given address
    /// This will adjust the `FpRev1CompX.comp() result based on the `FpRev1CompX.replace()` specification
    /// NOTE: Does not support a `replace value of '11'
    fn get_breakpoint_comparator(register_value: u32) -> Result<u32, Error> {
        let fp1_val = FpRev1CompX::from(register_value);
        if fp1_val.replace() == 0b01 {
            Ok(fp1_val.comp() << 2)
        } else if fp1_val.replace() == 0b10 {
            Ok((fp1_val.comp() << 2) | 0x2)
        } else {
            Err(Error::Arm(ArmError::Other(format!("Unsupported breakpoint comparator value {:#08x} for HW breakpoint. Breakpoint must be on half-word boundaries", fp1_val.0))))
        }
    }
    /// Get the correct register configuration which enables
    /// a hardware breakpoint at the given address.
    /// NOTE: Does not support a `replace` value of '11'
    pub(crate) fn breakpoint_configuration(address: u32) -> Result<Self, ArmError> {
        let mut reg = FpRev1CompX::from(0);

        // The highest 3 bits of the address have to be zero, otherwise the breakpoint cannot
        // be set at the address.
        if address >= 0x2000_0000 {
            return Err(ArmError::UnsupportedBreakpointAddress(address));
        }

        let comp_val = (address & 0x1f_ff_ff_fc) >> 2;

        // the replace value decides if the upper or lower half
        // word is matched for the break point
        let replace_val = if (address & 0x3) == 0 {
            0b01 // lower half word
        } else {
            0b10 // upper half word
        };

        reg.set_replace(replace_val);
        reg.set_comp(comp_val);
        reg.set_enable(true);

        Ok(reg)
    }
}

bitfield! {
    /// The FP_COMPn register bit assignments for FPB Version 2 where the Flash Patch is not implemented (see [`FpRev1CompX`]).
    #[derive(Copy,Clone)]
    pub struct FpRev2CompX(u32);
    impl Debug;
    /// BPADDR, `bits[31:1]` Breakpoint address. Specifies `bits[31:1]` of the breakpoint instruction address.
    ///
    /// If `BE == 0`, this field is Reserved, UNK/SBZP.
    ///
    /// The reset value of this field is UNKNOWN.
    pub bpaddr, set_bpaddr: 31, 1;
    /// Enable bit for breakpoint:
    ///
    /// `0`: Breakpoint disabled.\
    /// `1`: Breakpoint enabled.
    ///
    /// The reset value of this bit is UNKNOWN.
    pub enable, set_enable: 0;
}

impl MemoryMappedRegister<u32> for FpRev2CompX {
    const ADDRESS_OFFSET: u64 = 0xE000_2008;
    const NAME: &'static str = "FP_CTRL";
}

impl From<u32> for FpRev2CompX {
    fn from(value: u32) -> Self {
        FpRev2CompX(value)
    }
}

impl From<FpRev2CompX> for u32 {
    fn from(value: FpRev2CompX) -> Self {
        value.0
    }
}

impl FpRev2CompX {
    /// Get the correct register configuration which enables
    /// a hardware breakpoint at the given address.
    pub(crate) fn breakpoint_configuration(address: u32) -> Self {
        let mut reg = FpRev2CompX::from(0);

        reg.set_bpaddr(address >> 1);
        reg.set_enable(true);

        reg
    }
}

/// The state of a core that can be used to persist core state across calls to multiple different cores.
pub struct Armv7m<'probe> {
    memory: Box<dyn ArmMemoryInterface + 'probe>,

    state: &'probe mut CortexMState,

    sequence: Arc<dyn ArmDebugSequence>,
}

impl<'probe> Armv7m<'probe> {
    pub(crate) fn new(
        mut memory: Box<dyn ArmMemoryInterface + 'probe>,
        state: &'probe mut CortexMState,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Self, Error> {
        if !state.initialized() {
            // determine current state
            let dhcsr = Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?);

            let core_state = if dhcsr.s_sleep() {
                CoreStatus::Sleeping
            } else if dhcsr.s_halt() {
                let dfsr = Dfsr(memory.read_word_32(Dfsr::get_mmio_address())?);

                let reason = dfsr.halt_reason();

                tracing::debug!("Core was halted when connecting, reason: {:?}", reason);

                CoreStatus::Halted(reason)
            } else {
                CoreStatus::Running
            };

            // Clear DFSR register. The bits in the register are sticky,
            // so we clear them here to ensure that that none are set.
            let dfsr_clear = Dfsr::clear_all();

            memory.write_word_32(Dfsr::get_mmio_address(), dfsr_clear.into())?;

            state.current_state = core_state;
            state.fp_present = Mvfr0(memory.read_word_32(Mvfr0::get_mmio_address())?).fp_present();

            state.initialize();
        }

        Ok(Self {
            memory,
            state,
            sequence,
        })
    }

    fn set_core_status(&mut self, new_status: CoreStatus) {
        super::update_core_status(&mut self.memory, &mut self.state.current_state, new_status);
    }
}

impl<'probe> CoreInterface for Armv7m<'probe> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        // Wait until halted state is active again.
        let start = Instant::now();

        while !self.core_halted()? {
            if start.elapsed() >= timeout {
                return Err(Error::Arm(ArmError::Timeout));
            }
            // Wait a bit before polling again.
            std::thread::sleep(Duration::from_millis(1));
        }

        Ok(())
    }

    fn core_halted(&mut self) -> Result<bool, Error> {
        Ok(self.status()?.is_halted())
    }

    fn status(&mut self) -> Result<CoreStatus, Error> {
        let dhcsr = Dhcsr(self.memory.read_word_32(Dhcsr::get_mmio_address())?);

        if dhcsr.s_lockup() {
            tracing::error!(
                "The core is in locked up status as a result of an unrecoverable exception"
            );

            self.set_core_status(CoreStatus::LockedUp);

            return Ok(CoreStatus::LockedUp);
        }

        if dhcsr.s_sleep() {
            // Check if we assumed the core to be halted
            if self.state.current_state.is_halted() {
                tracing::warn!("Expected core to be halted, but core is running");
            }

            self.set_core_status(CoreStatus::Sleeping);

            return Ok(CoreStatus::Sleeping);
        }

        if dhcsr.s_halt() {
            let dfsr = Dfsr(self.memory.read_word_32(Dfsr::get_mmio_address())?);

            let mut reason = dfsr.halt_reason();

            // Clear bits from Dfsr register
            self.memory
                .write_word_32(Dfsr::get_mmio_address(), Dfsr::clear_all().into())?;

            // If the core was halted before, we cannot read the halt reason from the chip,
            // because we clear it directly after reading.
            if self.state.current_state.is_halted() {
                // There shouldn't be any bits set, otherwise it means
                // that the reason for the halt has changed. No bits set
                // means that we have an unkown HaltReason.
                if reason == HaltReason::Unknown {
                    tracing::debug!("Cached halt reason: {:?}", self.state.current_state);
                    return Ok(self.state.current_state);
                }

                tracing::debug!(
                    "Reason for halt has changed, old reason was {:?}, new reason is {:?}",
                    &self.state.current_state,
                    &reason
                );
            }

            // Set the status so any semihosting operations will know we're halted
            self.set_core_status(CoreStatus::Halted(reason));

            if let HaltReason::Breakpoint(_) = reason {
                self.state.semihosting_command = super::cortex_m::check_for_semihosting(
                    self.state.semihosting_command.take(),
                    self,
                )?;
                if let Some(command) = self.state.semihosting_command {
                    reason = HaltReason::Breakpoint(BreakpointCause::Semihosting(command));
                }

                // Set it again if it's changed
                self.set_core_status(CoreStatus::Halted(reason));
            }

            return Ok(CoreStatus::Halted(reason));
        }

        // Core is neither halted nor sleeping, so we assume it is running.
        if self.state.current_state.is_halted() {
            tracing::warn!("Core is running, but we expected it to be halted");
        }

        self.set_core_status(CoreStatus::Running);

        Ok(CoreStatus::Running)
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        // TODO: Generic halt support

        let mut value = Dhcsr(0);
        value.set_c_halt(true);
        value.set_c_debugen(true);
        value.enable_write();

        self.memory
            .write_word_32(Dhcsr::get_mmio_address(), value.into())?;

        self.wait_for_core_halted(timeout)?;

        // try to read the program counter
        let pc_value = self.read_core_reg(self.program_counter().into())?;

        // get pc
        Ok(CoreInformation {
            pc: pc_value.try_into()?,
        })
    }

    fn run(&mut self) -> Result<(), Error> {
        // Before we run, we always perform a single instruction step, to account for possible breakpoints that might get us stuck on the current instruction.
        self.step()?;

        let mut dhcsr = Dhcsr(self.memory.read_word_32(Dhcsr::get_mmio_address())?);

        // First disable the DHCSR->C_MASKINTS.
        if dhcsr.c_maskints() {
            dhcsr.set_c_maskints(false);
            dhcsr.enable_write();
            self.memory
                .write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
            self.memory.flush()?;
        }

        // Exit halt state ..
        dhcsr.set_c_step(false);
        dhcsr.set_c_halt(false);
        dhcsr.enable_write();
        self.memory
            .write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
        self.memory.flush()?;

        // We assume that the core is running now
        self.set_core_status(CoreStatus::Running);

        Ok(())
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.state.semihosting_command = None;

        self.sequence
            .reset_system(&mut *self.memory, crate::CoreType::Armv7m, None)?;
        Ok(())
    }

    fn reset_and_halt(&mut self, _timeout: Duration) -> Result<CoreInformation, Error> {
        // Set the vc_corereset bit in the DEMCR register.
        // This will halt the core after reset.
        self.reset_catch_set()?;

        self.sequence
            .reset_system(&mut *self.memory, crate::CoreType::Armv7m, None)?;

        // Update core status
        let _ = self.status()?;

        const XPSR_THUMB: u32 = 1 << 24;

        let xpsr_value: u32 = self.read_core_reg(XPSR.id())?.try_into()?;
        if xpsr_value & XPSR_THUMB == 0 {
            self.write_core_reg(XPSR.id(), (xpsr_value | XPSR_THUMB).into())?;
        }

        self.reset_catch_clear()?;

        // try to read the program counter
        let pc_value = self.read_core_reg(self.program_counter().into())?;

        // get pc
        Ok(CoreInformation {
            pc: pc_value.try_into()?,
        })
    }

    fn step(&mut self) -> Result<CoreInformation, Error> {
        // First check if we stopped on a breakpoint, because this requires special handling before we can continue.
        let breakpoint_at_pc = if matches!(
            self.state.current_state,
            CoreStatus::Halted(HaltReason::Breakpoint(_))
        ) {
            let pc_before_step = self.read_core_reg(self.program_counter().into())?;
            self.enable_breakpoints(false)?;
            Some(pc_before_step)
        } else {
            None
        };

        let mut dhcsr = Dhcsr(self.memory.read_word_32(Dhcsr::get_mmio_address())?);

        // Follow the rules of the ... ARMv7-M Architecture reference, C1.6 Debug System Registers - DHCSR, with respect to setting maskints
        if !dhcsr.c_debugen() {
            tracing::warn!("Attempting to STEP while DHCSR->C_DEBUGEN is false");
        }
        if !dhcsr.c_maskints() {
            dhcsr.set_c_maskints(true); // This must be reset to false when we run() again.
            dhcsr.enable_write();
            self.memory
                .write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
            self.memory.flush()?;
        }

        // Leave halted state.
        // Step one instruction.
        dhcsr.set_c_step(true);
        dhcsr.set_c_halt(false);
        dhcsr.enable_write();
        self.memory
            .write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
        self.memory.flush()?;

        self.wait_for_core_halted(Duration::from_millis(100))?;

        // Try to read the new program counter.
        let mut pc_after_step = self.read_core_reg(self.program_counter().into())?;

        // Re-enable breakpoints before we continue.
        if let Some(pc_before_step) = breakpoint_at_pc {
            // If we were stopped on a software breakpoint, then we need to manually advance the PC, or else we will be stuck here forever.
            if pc_before_step == pc_after_step
                && !self
                    .hw_breakpoints()?
                    .contains(&pc_before_step.try_into().ok())
            {
                tracing::debug!("Encountered a breakpoint instruction @ {}. We need to manually advance the program counter to the next instruction.", pc_after_step);
                // Advance the program counter by the architecture specific byte size of the BKPT instruction.
                pc_after_step.increment_address(2)?;
                self.write_core_reg(self.program_counter().into(), pc_after_step)?;
            }
            self.enable_breakpoints(true)?;
        }

        self.state.semihosting_command = None;

        Ok(CoreInformation {
            pc: pc_after_step.try_into()?,
        })
    }

    fn read_core_reg(&mut self, address: RegisterId) -> Result<RegisterValue, Error> {
        if self.state.current_state.is_halted() {
            let val = super::cortex_m::read_core_reg(&mut *self.memory, address)?;
            Ok(val.into())
        } else {
            Err(Error::Arm(ArmError::CoreNotHalted))
        }
    }

    fn write_core_reg(&mut self, address: RegisterId, value: RegisterValue) -> Result<(), Error> {
        if self.state.current_state.is_halted() {
            super::cortex_m::write_core_reg(&mut *self.memory, address, value.try_into()?)?;
            Ok(())
        } else {
            Err(Error::Arm(ArmError::CoreNotHalted))
        }
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        let raw_val = self.memory.read_word_32(FpCtrl::get_mmio_address())?;

        let reg = FpCtrl::from(raw_val);

        if reg.rev() == 0 || reg.rev() == 1 {
            Ok(reg.num_code())
        } else {
            tracing::warn!("This chip uses FPBU revision {}, which is not yet supported. HW breakpoints are not available.", reg.rev());
            Err(
                Error::Arm(ArmError::Other(format!("This chip uses FPBU revision {}, which is not yet supported. HW breakpoints are not available.", reg.rev())))
            )
        }
    }

    /// See docs on the [`CoreInterface::hw_breakpoints`] trait.
    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        let mut breakpoints = vec![];
        let num_hw_breakpoints = self.available_breakpoint_units()? as usize;
        { 0..num_hw_breakpoints }.try_for_each(|bp_unit_index| {
            let raw_val = self.memory.read_word_32(FpCtrl::get_mmio_address())?;
            let ctrl_reg = FpCtrl::from(raw_val);
            // FpRev1 and FpRev2 needs different decoding of the register value, but the location where we read from is the same ...
            let reg_addr = FpRev1CompX::get_mmio_address() + (bp_unit_index * size_of::<u32>()) as u64;
            // The raw breakpoint address as read from memory.
            let register_value = self.memory.read_word_32(reg_addr)?;
            // The breakpoint address after it has been adjusted for FpRev 1 or 2.
            let breakpoint:u32;
            if register_value & 0b1 == 0b1 {
                // We only care about `enabled` breakpoints.
                if ctrl_reg.rev() == 0 {
                    breakpoint = FpRev1CompX::get_breakpoint_comparator(register_value)?;
                } else if ctrl_reg.rev() == 1 {
                    breakpoint = FpRev2CompX::from(register_value).bpaddr() << 1;
                } else {
                    tracing::warn!("This chip uses FPBU revision {}, which is not yet supported. HW breakpoints are not available.", ctrl_reg.rev());
                    return Err(Error::Other(format!("This chip uses FPBU revision {}, which is not yet supported. HW breakpoints are not available.", ctrl_reg.rev())));
                }
                breakpoints.push(Some(breakpoint as u64));
            } else {
                breakpoints.push(None);
            }
            Ok(())
        })?;
        Ok(breakpoints)
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), Error> {
        let mut val = FpCtrl::from(0);
        val.set_key(true);
        val.set_enable(state);

        self.memory
            .write_word_32(FpCtrl::get_mmio_address(), val.into())?;
        self.memory.flush()?;

        self.state.hw_breakpoints_enabled = state;

        Ok(())
    }

    fn set_hw_breakpoint(&mut self, bp_unit_index: usize, addr: u64) -> Result<(), Error> {
        let addr = valid_32bit_address(addr)?;

        // First make sure they are asking for a breakpoint on a half-word boundary.
        if (addr & 0x1) > 0 {
            return Err(Error::Other(format!(
                "The requested breakpoint address 0x{:08x} is not on a half-word boundary",
                addr
            )));
        }

        let raw_val = self.memory.read_word_32(FpCtrl::get_mmio_address())?;
        let ctrl_reg = FpCtrl::from(raw_val);

        let val: u32;
        if ctrl_reg.rev() == 0 {
            val = FpRev1CompX::breakpoint_configuration(addr)?.into();
        } else if ctrl_reg.rev() == 1 {
            val = FpRev2CompX::breakpoint_configuration(addr).into();
        } else {
            tracing::warn!("This chip uses FPBU revision {}, which is not yet supported. HW breakpoints are not available.", ctrl_reg.rev());
            return Err(Error::Other(format!("This chip uses FPBU revision {}, which is not yet supported. HW breakpoints are not available.", ctrl_reg.rev())));
        }

        // This is fine as FpRev1CompX and Rev2CompX are just two different
        // interpretations of the same memory region as Rev2 can handle bigger
        // address spaces than Rev1.
        let reg_addr = FpRev1CompX::get_mmio_address() + (bp_unit_index * size_of::<u32>()) as u64;

        self.memory.write_word_32(reg_addr, val)?;

        Ok(())
    }

    fn clear_hw_breakpoint(&mut self, bp_unit_index: usize) -> Result<(), Error> {
        let mut val = FpRev1CompX::from(0);
        val.set_enable(false);

        let reg_addr = FpRev1CompX::get_mmio_address() + (bp_unit_index * size_of::<u32>()) as u64;

        self.memory.write_word_32(reg_addr, val.into())?;

        Ok(())
    }

    fn registers(&self) -> &'static CoreRegisters {
        if self.state.fp_present {
            &CORTEX_M_WITH_FP_CORE_REGISTERS
        } else {
            &CORTEX_M_CORE_REGISTERS
        }
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
        self.state.hw_breakpoints_enabled
    }

    fn architecture(&self) -> Architecture {
        Architecture::Arm
    }

    fn core_type(&self) -> CoreType {
        CoreType::Armv7m
    }

    fn instruction_set(&mut self) -> Result<InstructionSet, Error> {
        Ok(InstructionSet::Thumb2)
    }

    fn fpu_support(&mut self) -> Result<bool, Error> {
        Ok(self.state.fp_present)
    }

    fn floating_point_register_count(&mut self) -> Result<usize, Error> {
        Ok(32)
    }

    #[tracing::instrument(skip(self))]
    fn reset_catch_set(&mut self) -> Result<(), Error> {
        self.sequence
            .reset_catch_set(&mut *self.memory, CoreType::Armv7m, None)?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn reset_catch_clear(&mut self) -> Result<(), Error> {
        self.sequence
            .reset_catch_clear(&mut *self.memory, CoreType::Armv7m, None)?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn debug_core_stop(&mut self) -> Result<(), Error> {
        self.sequence
            .debug_core_stop(&mut *self.memory, CoreType::Armv7m)?;
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn enable_vector_catch(&mut self, condition: VectorCatchCondition) -> Result<(), Error> {
        let mut dhcsr = Dhcsr(self.memory.read_word_32(Dhcsr::get_mmio_address())?);
        dhcsr.set_c_debugen(true);
        self.memory
            .write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;

        let mut demcr = Demcr(self.memory.read_word_32(Demcr::get_mmio_address())?);
        match condition {
            VectorCatchCondition::HardFault => demcr.set_vc_harderr(true),
            VectorCatchCondition::CoreReset => demcr.set_vc_corereset(true),
            VectorCatchCondition::SecureFault => {
                return Err(Error::Arm(ArmError::ArchitectureRequired(&["ARMv8"])));
            }
            VectorCatchCondition::All => {
                demcr.set_vc_harderr(true);
                demcr.set_vc_corereset(true);
            }
        };

        self.memory
            .write_word_32(Demcr::get_mmio_address(), demcr.into())?;
        Ok(())
    }

    fn disable_vector_catch(&mut self, condition: VectorCatchCondition) -> Result<(), Error> {
        let mut demcr = Demcr(self.memory.read_word_32(Demcr::get_mmio_address())?);
        match condition {
            VectorCatchCondition::HardFault => demcr.set_vc_harderr(false),
            VectorCatchCondition::CoreReset => demcr.set_vc_corereset(false),
            VectorCatchCondition::SecureFault => {
                return Err(Error::Arm(ArmError::ArchitectureRequired(&["ARMv8"])));
            }
            VectorCatchCondition::All => {
                demcr.set_vc_harderr(false);
                demcr.set_vc_corereset(false);
            }
        };

        self.memory
            .write_word_32(Demcr::get_mmio_address(), demcr.into())?;
        Ok(())
    }
}

impl super::CoreMemoryInterface for Armv7m<'_> {
    fn memory(&self) -> &dyn ArmMemoryInterface {
        &*self.memory
    }
    fn memory_mut(&mut self) -> &mut dyn ArmMemoryInterface {
        &mut *self.memory
    }
}

#[test]
fn breakpoint_register_value() {
    // Check that the register configuration for the FPBU is
    // calculated correctly.
    //
    // See ARMv7 Architecture Reference Manual, Section C1.11.5
    let address: u32 = 0x0800_09A4;

    let reg = FpRev1CompX::breakpoint_configuration(address).unwrap();
    let reg_val: u32 = reg.into();

    assert_eq!(0x4800_09A5, reg_val);
}

#[test]
fn unsupported_breakpoint_address() {
    // Revision 1 of the FPBU only supports breakpoints for address < 0x2000_0000.
    let address: u32 = 0x2000_0000;

    FpRev1CompX::breakpoint_configuration(address).unwrap_err();
}
