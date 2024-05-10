//! Sequences for ATSAML10/L11 target families
//!
//! ATSAML10/L11 devices are Cortex-M23 (ARMv8-M Baseline) based microcontrollers,
//! using the ARM Debug Interface Architecture v5 Specification. L11 devices include
//! the ARM TrustZone security technology.

use crate::{
    architecture::arm::{
        ap::MemoryAp,
        armv8m::Dhcsr,
        communication_interface::{DapProbe, SwdSequence},
        memory::adi_v5_memory_interface::ArmProbe,
        sequences::{ArmDebugSequence, ArmDebugSequenceError},
        ArmError, ArmProbeInterface, Pins,
    },
    probe::DebugProbeError,
    MemoryMappedRegister,
};
use bitfield::bitfield;
use probe_rs_target::CoreType;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;

bitfield! {
    /// Device Service Unit Control Register, DSU - CTRL
    #[derive(Copy, Clone)]
    pub struct DsuCtrl(u8);
    impl Debug;
    /// Memory Built-In Self-Test
    ///
    /// Writing a '0' to this bit has no effect.\
    /// Writing a '1' to this bit starts the memory BIST algorithm.
    pub _, set_mbist: 3;
    /// 32-bit Cyclic Redundancy Check
    ///
    /// Writing a '0' to this bit has no effect.\
    /// Writing a '1' to this bit starts the cyclic redundancy check algorithm.
    pub _, set_crc: 2;
    /// Software Reset
    ///
    /// Writing a '0' to this bit has no effect.\
    /// Writing a '1' to this bit resets the module.
    pub _, set_swrst: 0;
}

impl DsuCtrl {
    /// The DSU CTRL register address
    pub const ADDRESS: u64 = 0x4100_2100;
}

impl From<u8> for DsuCtrl {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

impl From<DsuCtrl> for u8 {
    fn from(value: DsuCtrl) -> Self {
        value.0
    }
}

bitfield! {
    /// Device Service Unit Status A Register, DSU - STATUSA
    #[derive(Copy, Clone)]
    pub struct DsuStatusA(u8);
    impl Debug;
    /// Boot ROM Phase Extension
    ///
    /// This bit is set when a debug adapter Cold-Plugging is detected, which extends the Boot ROM
    /// phase.
    ///
    /// Writing a '0' to this bit has no effect.
    /// Writing a '1' to this bit clears the Boot ROM Phase Extension bit
    pub brext, set_brext: 5;

    /// Protection Error
    ///
    /// This bit is set when a command that is not allowed in Protected state is issued.
    ///
    /// Writing a '0' to this bit has no effect.
    /// Writing a '1' to this bit clears the Protection Error bit.
    pub perr, set_perr: 4;

    /// Failure
    ///
    /// This bit is set when a DSU operation failure is detected.
    ///
    /// Writing a '0' to this bit has no effect.
    /// Writing a '1' to this bit clears the Failure bit.
    pub fail, set_fail: 3;

    /// Bus Error
    ///
    /// This bit is set when a bus error is detected.
    ///
    /// Writing a '0' to this bit has no effect.
    /// Writing a '1' to this bit clears the Bus Error bit.
    pub berr, set_berr: 2;

    /// CPU Reset Phase Extension
    ///
    /// This bit is set when a debug adapter Cold-Plugging is detected, which extends the CPU Reset
    /// phase.
    ///
    /// Writing a '0' to this bit has no effect.
    /// Writing a '1' to this bit clears the CPU Reset Phase Extension bit.
    pub crstext, set_crstext: 1;

    /// Done
    ///
    /// This bit is set when a DSU operation is completed.
    ///
    /// Writing a '0' to this bit has no effect.
    /// Writing a '1' to this bit clears the Done bit.
    pub done, set_done: 0;
}

impl From<u8> for DsuStatusA {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

impl From<DsuStatusA> for u8 {
    fn from(value: DsuStatusA) -> Self {
        value.0
    }
}

impl DsuStatusA {
    /// The DSU STATUSA register address
    pub const ADDRESS: u64 = 0x4100_2101;
}

bitfield! {
    /// Device Service Unit Status B Register, DSU - STATUSB
    #[derive(Copy, Clone)]
    pub struct DsuStatusB(u8);
    impl Debug;

    /// BOOT Communication Channel 1 Dirty
    ///
    /// This bit is set when BCC1 is written.
    /// This bit is cleared when BCC1 is read.
    pub bccd1, _: 7;

    /// BOOT Communication Channel 0 Dirty
    ///
    /// This bit is set when BCC0 is written.
    /// This bit is cleared when BCC0 is read.
    pub bccd0, _: 6;

    /// Debug Communication Channel 1 Dirty
    ///
    /// This bit is set when DCC1 is written.
    /// This bit is cleared when DCC1 is read.
    pub dccd1, _: 5;

    /// Debug Communication Channel 0 Dirty
    ///
    /// This bit is set when DCC0 is written.
    /// This bit is cleared when DCC0 is read.
    pub dccd0, _: 4;

    /// Hot-Plugging Enable
    ///
    /// This bit is set when Hot-Plugging is enabled.
    /// This bit is cleared when Hot-Plugging is disabled. This is the case when
    /// the SWCLK function is changed. Only a power-reset or a external reset
    /// can set it again.
    pub hpe, _: 3;

    /// Debugger Present
    ///
    /// This bit is set when a debugger probe is detected.
    /// This bit is never cleared.
    pub dbgpres, _: 2;

    /// Debugger Access Level
    ///
    /// Indicates the debugger access level:
    /// - 0x0: Debugger can only access the DSU external address space.
    /// - 0x1: Debugger can access only Non-Secure regions (SAM L11 only).
    /// - 0x2: Debugger can access secure and Non-Secure regions.
    /// Writing in this bitfield has no effect.
    pub dal, _: 1, 0;

}

impl From<u8> for DsuStatusB {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

impl From<DsuStatusB> for u8 {
    fn from(value: DsuStatusB) -> Self {
        value.0
    }
}

impl DsuStatusB {
    /// The DSU STATUSB register address
    pub const ADDRESS: u64 = 0x4100_2102;
}

/// A wrapper for different types that can perform SWD Commands (SWJ_Pins SWJ_Sequence)
struct SwdSequenceShim<'a>(&'a mut dyn DapProbe);

impl<'a> From<&'a mut dyn DapProbe> for SwdSequenceShim<'a> {
    fn from(p: &'a mut dyn DapProbe) -> Self {
        Self(p)
    }
}

impl<'a> SwdSequence for SwdSequenceShim<'a> {
    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError> {
        self.0.swj_sequence(bit_len, bits)
    }

    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        self.0.swj_pins(pin_out, pin_select, pin_wait)
    }
}

/// Marker struct indicating initialization sequencing for Atmel/Microchip ATSAM family parts.
#[derive(Debug)]
pub struct AtSAML1x {}

impl AtSAML1x {
    /// Create the sequencer for the ATSAM family of parts.
    pub fn create() -> Arc<Self> {
        Arc::new(Self {})
    }

    /// Perform a hardware reset in a way that puts the core into CPU Reset Extension
    ///
    /// CPU Reset Extension is a vendor specific feature that allows the CPU core to remain
    /// in reset while the rest of the debugging subsystem can run and initialize itself.
    ///
    /// For more details see: 12.6.2 CPU Reset Extension in the SAM D5/E5 Family Data Sheet
    ///
    /// # Errors
    /// Subject to probe communication errors
    pub fn reset_hardware_with_extension(
        &self,
        interface: &mut dyn SwdSequence,
    ) -> Result<(), ArmError> {
        let mut pins = Pins(0);
        pins.set_nreset(true);
        pins.set_swdio_tms(true);
        pins.set_swclk_tck(true);

        // First set nReset, SWDIO and SWCLK to low.
        let mut pin_values = Pins(0);
        interface.swj_pins(pin_values.0 as u32, pins.0 as u32, 0)?;
        std::thread::sleep(Duration::from_millis(10));

        // Next release nReset, but keep SWDIO and SWCLK low. This should put the device into
        // reset extension.
        pin_values.set_nreset(true);
        interface.swj_pins(pin_values.0 as u32, pins.0 as u32, 0)?;
        std::thread::sleep(Duration::from_millis(20));

        Ok(())
    }

    /// Release the CPU core from Reset Extension
    ///
    /// Clear the DSU Reset Extension bit, which releases the core from reset extension.
    ///
    /// # Errors
    /// Subject to probe communication errors
    pub fn release_reset_extension(&self, memory: &mut dyn ArmProbe) -> Result<(), ArmError> {
        // Enable debug mode if it is not already enabled
        let mut dhcsr = Dhcsr(0);
        dhcsr.enable_write();
        dhcsr.set_c_debugen(true);
        memory.write_word_32(Dhcsr::ADDRESS_OFFSET, dhcsr.0)?;

        // clear the reset extension bit
        let mut dsu_statusa = DsuStatusA(0);
        dsu_statusa.set_crstext(true);
        memory.write_8(DsuStatusA::ADDRESS, &[dsu_statusa.0])?;

        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(100) {
            let current_dsu_statusa = DsuStatusA::from(memory.read_word_8(DsuStatusA::ADDRESS)?);
            if !current_dsu_statusa.crstext() {
                return Ok(());
            }
        }

        Err(ArmError::Timeout)
    }

    /// Perform a normal hardware reset without triggering a Reset extension
    ///
    /// # Errors
    /// Subject to probe communication errors
    pub fn reset_hardware(&self, interface: &mut dyn SwdSequence) -> Result<(), ArmError> {
        let mut pins = Pins(0);
        pins.set_nreset(true);
        pins.set_swdio_tms(true);
        pins.set_swclk_tck(true);

        let mut pin_values = pins;

        pin_values.set_nreset(false);
        interface.swj_pins(pin_values.0 as u32, pins.0 as u32, 0)?;
        std::thread::sleep(Duration::from_millis(10));

        pin_values.set_nreset(true);
        interface.swj_pins(pin_values.0 as u32, pins.0 as u32, 0)?;
        std::thread::sleep(Duration::from_millis(10));

        Ok(())
    }

    /// Some probes only support setting `nRESET` directly, but not the SWDIO/SWCLK pins. For those,
    /// we can use this fallback method.
    fn ensure_target_reset(
        &self,
        result: Result<(), ArmError>,
        interface: &mut dyn SwdSequence,
    ) -> Result<(), ArmError> {
        // Fall back if the probe's swj_pins does not support the full set of pins
        match result {
            Err(ArmError::Probe(DebugProbeError::CommandNotSupportedByProbe {
                command_name: "swj_pins",
            })) => {
                tracing::warn!("Using fallback reset method");

                let mut pins = Pins(0);
                pins.set_nreset(true);

                interface.swj_pins(0, pins.0 as u32, 0)?;
                std::thread::sleep(Duration::from_millis(10));

                interface.swj_pins(pins.0 as u32, pins.0 as u32, 0)?;
                std::thread::sleep(Duration::from_millis(10));

                Ok(())
            }
            other => other,
        }
    }
}

impl ArmDebugSequence for AtSAML1x {
    fn debug_core_start(
        &self,
        interface: &mut dyn ArmProbeInterface,
        core_ap: MemoryAp,
        _core_type: CoreType,
        _debug_base: Option<u64>,
        _cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut core = interface.memory_interface(core_ap)?;

        self.release_reset_extension(&mut *core)
    }

    /// `reset_hardware_assert` for ATSAM devices
    ///
    /// Instead of keeping `nReset` asserted, the device is instead put into CPU Reset Extension
    /// which will keep the CPU Core in reset until manually released by the debugger probe.
    fn reset_hardware_assert(&self, interface: &mut dyn DapProbe) -> Result<(), ArmError> {
        let mut shim = SwdSequenceShim::from(interface);
        let result = self.reset_hardware_with_extension(&mut shim);

        self.ensure_target_reset(result, &mut shim)
    }

    /// `reset_hardware_deassert` for ATSAM devices
    ///
    /// Instead of de-asserting `nReset` here (this was already done during the CPU Reset Extension process),
    /// the device is released from Reset Extension.
    fn reset_hardware_deassert(&self, memory: &mut dyn ArmProbe) -> Result<(), ArmError> {
        let mut pins = Pins(0);
        pins.set_nreset(true);

        let current_pins = Pins(memory.swj_pins(pins.0 as u32, pins.0 as u32, 0)? as u8);
        if !current_pins.nreset() {
            return Err(ArmDebugSequenceError::SequenceSpecific(
                "Expected nReset to already be de-asserted".into(),
            )
            .into());
        }

        self.release_reset_extension(memory)
    }
}
