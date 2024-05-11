//! Sequences for ATSAML10/L11 target families
//!
//! ATSAML10/L11 devices are Cortex-M23 (ARMv8-M Baseline) based microcontrollers,
//! using the ARM Debug Interface Architecture v5 Specification. L11 devices include
//! the ARM TrustZone security technology.

use crate::{
    architecture::arm::{
        ap::{AccessPortError, MemoryAp},
        armv8m::{Aircr, Dhcsr},
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
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
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

/// Boot Communication Channel 0 Register, DSU - BCC0 (Debugger to device)
#[derive(Clone, Copy, Debug, PartialEq)]
struct DsuBcc0(u32);

impl From<u32> for DsuBcc0 {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<DsuBcc0> for u32 {
    fn from(value: DsuBcc0) -> Self {
        value.0
    }
}

#[allow(dead_code)]
impl DsuBcc0 {
    /// The DSU BCC0 register address
    pub const ADDRESS: u64 = 0x4100_2120;

    /// Entering Interactive Mode
    pub const CMD_INIT: Self = Self(0x44424755);
    /// Exit Interactive Mode
    pub const CMD_EXIT: Self = Self(0x444247AA);
    /// System Reset Request
    pub const CMD_RESET: Self = Self(0x44424752);
    /// ChipErase_NS for SAM L11
    pub const CMD_CE0: Self = Self(0x444247E0);
    /// ChipErase_S for SAM L11
    pub const CMD_CE1: Self = Self(0x444247E1);
    /// ChipErase_ALL for SAM L11
    pub const CMD_CE2: Self = Self(0x444247E2);
    /// ChipErase for SAM L10
    pub const CMD_CHIPERASE: Self = Self(0x444247E3);
    /// NVM Memory Regions Integrity Checks
    pub const CMD_CRC: Self = Self(0x444247C0);
    ///  Random Session Key Generation for SAM L11
    pub const CMD_DCEK: Self = Self(0x44424744);
    /// NVM Rows Integrity Checks
    pub const CMD_RAUX: Self = Self(0x4442474C);
}

/// Boot Communication Channel 1 Register, DSU - BCC1 (Device to debugger)
#[derive(Clone, Copy, Debug, PartialEq)]
struct DsuBcc1(u32);

impl From<u32> for DsuBcc1 {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<DsuBcc1> for u32 {
    fn from(value: DsuBcc1) -> Self {
        value.0
    }
}

#[allow(dead_code)]
impl DsuBcc1 {
    /// The DSU BCC1 register address
    pub const ADDRESS: u64 = 0x4100_2124;

    /// No Error
    pub const SIG_NO: Self = Self(0xEC000000);
    /// Fresh from factory error
    pub const SIG_SAN_FFF: Self = Self(0xEC000010);
    /// UROW checksum error
    pub const SIG_SAN_UROW: Self = Self(0xEC000011);
    /// SECEN parameter error
    pub const SIG_SAN_SECEN: Self = Self(0xEC000012);
    /// BOCOR checksum error
    pub const SIG_SAN_BOCOR: Self = Self(0xEC000013);
    /// BOOTPROT parameter error
    pub const SIG_SAN_BOOTPROT: Self = Self(0xEC000014);
    /// No secure register parameter error
    pub const SIG_SAN_NOSECREG: Self = Self(0xEC000015);
    /// Debugger start communication command
    pub const SIG_COMM: Self = Self(0xEC000020);
    /// Debugger command success
    pub const SIG_CMD_SUCCESS: Self = Self(0xEC000021);
    /// Debugger command fail
    pub const SIG_CMD_FAIL: Self = Self(0xEC000022);
    /// Debugger bad key
    pub const SIG_CMD_BADKEY: Self = Self(0xEC000023);
    /// Valid command
    pub const SIG_CMD_VALID: Self = Self(0xEC000024);
    /// Invalid command
    pub const SIG_CMD_INVALID: Self = Self(0xEC000025);
    /// Valid argument
    pub const SIG_ARG_VALID: Self = Self(0xEC000026);
    /// Invalid argument
    pub const SIG_ARG_INVALID: Self = Self(0xEC000027);
    /// Chip erase error: CVM
    pub const SIG_CE_CVM: Self = Self(0xEC000030);
    /// Chip erase error: array erase fail
    pub const SIG_CE_ARRAY_ERASEFAIL: Self = Self(0xEC000031);
    /// Chip erase error: array NVME
    pub const SIG_CE_ARRAY_NVME: Self = Self(0xEC000032);
    /// Chip erase error: data erase fail
    pub const SIG_CE_DATA_ERASEFAIL: Self = Self(0xEC000033);
    /// Chip erase error: data NVME
    pub const SIG_CE_DATA_NVME: Self = Self(0xEC000034);
    /// Chip erase error: BOCOR, UROW
    pub const SIG_CE_BCUR: Self = Self(0xEC000035);
    /// Chip erase error: BC check
    pub const SIG_CE_BC: Self = Self(0xEC000036);
    /// BOOTOPT parameter error
    pub const SIG_BOOT_OPT: Self = Self(0xEC000040);
    /// Boot image digest verify fail
    pub const SIG_BOOT_ERR: Self = Self(0xEC000041);
    /// BOCOR hash error
    pub const SIG_BOCOR_HASH: Self = Self(0xEC000042);
    /// Bad CRC table
    pub const SIG_CRC_BADTBL: Self = Self(0xEC000050);
    /// PAC or IDAU cfg check failure
    pub const SIG_SECEN0_ERR: Self = Self(0xEC000060);
    /// PAC or IDAU cfg check failure
    pub const SIG_SECEN1_ERR: Self = Self(0xEC000061);
    /// Exit: BC or check error
    pub const SIG_EXIT_ERR: Self = Self(0xEC000070);
    /// Hardfault error
    pub const SIG_HARDFAULT: Self = Self(0xEC0000F0);
    /// Boot ROM ok to exit
    pub const SIG_BOOTOK: Self = Self(0xEC000039);
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
pub struct AtSAML1x {
    /// We don't have the Main extension, so no reset vector catch for us.
    simulate_reset_catch: AtomicBool,
}

impl AtSAML1x {
    /// Create the sequencer for the ATSAM family of parts.
    pub fn create() -> Arc<Self> {
        Arc::new(Self {
            simulate_reset_catch: AtomicBool::new(false),
        })
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
        tracing::debug!("Releasing Reset Extension");

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

    fn exit_boot_rom_to_app(&self, memory: &mut dyn ArmProbe) -> Result<(), ArmError> {
        // Wait 5ms for the Boot ROM
        std::thread::sleep(Duration::from_millis(5));

        // Read STATUSB
        let statusb = DsuStatusB::from(memory.read_word_8(DsuStatusB::ADDRESS)?);
        tracing::info!("Debug Access Level: {}", statusb.dal());

        if !statusb.dbgpres() {
            tracing::warn!("Debugger not detected");
            return Err(ArmError::Other(anyhow::anyhow!(
                "Device does not detect debugger"
            )));
        }

        // Read error code from BCC1
        if statusb.bccd1() {
            let bcc1 = DsuBcc1::from(memory.read_word_32(DsuBcc1::ADDRESS)?);
            tracing::debug!("BCC1: {:#010x}", bcc1.0);
            if bcc1 != DsuBcc1::SIG_BOOTOK {
                tracing::warn!("Boot ROM error: {:#010x}", bcc1.0);
            }
        }

        // Clear BREXT
        let mut dsu_statusa = DsuStatusA(0);
        dsu_statusa.set_brext(true);
        memory.write_8(DsuStatusA::ADDRESS, &[dsu_statusa.0])?;

        // Write CMD_EXIT command to BCC0
        memory.write_word_32(DsuBcc0::ADDRESS, DsuBcc0::CMD_EXIT.0)?;

        // Read SIG_BOOTOK from BCC1
        let bcc1 = DsuBcc1::from(memory.read_word_32(DsuBcc1::ADDRESS)?);
        if bcc1 != DsuBcc1::SIG_BOOTOK {
            tracing::warn!("Boot ROM exit failed: {:#010x}", bcc1.0);
        }

        Ok(())
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
    fn reset_catch_set(
        &self,
        _: &mut dyn ArmProbe,
        _: probe_rs_target::CoreType,
        _: Option<u64>,
    ) -> Result<(), ArmError> {
        self.simulate_reset_catch.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn reset_catch_clear(
        &self,
        _: &mut dyn ArmProbe,
        _: probe_rs_target::CoreType,
        _: Option<u64>,
    ) -> Result<(), ArmError> {
        self.simulate_reset_catch.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmProbe,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> std::prelude::v1::Result<(), ArmError> {
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);

        interface.write_word_32(Aircr::get_mmio_address(), aircr.into())?;

        let start = Instant::now();

        while start.elapsed() < Duration::from_millis(500) {
            let dhcsr = match interface.read_word_32(Dhcsr::get_mmio_address()) {
                Ok(val) => Dhcsr(val),
                // Some combinations of debug probe and target (in
                // particular, hs-probe and ATSAMD21) result in
                // register read errors while the target is
                // resetting.
                Err(ArmError::AccessPort {
                    source: AccessPortError::RegisterRead { .. },
                    ..
                }) => continue,
                Err(err) => return Err(err),
            };

            // Wait until the S_RESET_ST bit is cleared on a read
            if !dhcsr.s_reset_st() {
                // Simulate the reset catch if it was enabled. We do this by parking the core through
                // the Boot ROM, halting it, then exiting the Boot ROM.
                if self.simulate_reset_catch.load(Ordering::Relaxed) {
                    self.exit_boot_rom_to_app(interface)?;

                    let mut dhcsr = Dhcsr(0);
                    dhcsr.enable_write();
                    dhcsr.set_c_debugen(true);
                    dhcsr.set_c_halt(true);
                    interface.write_word_32(Dhcsr::ADDRESS_OFFSET, dhcsr.0)?;

                    // Wait for the core to halt
                    let start = Instant::now();
                    loop {
                        let dhcsr = Dhcsr::from(interface.read_word_32(Dhcsr::ADDRESS_OFFSET)?);
                        if dhcsr.s_halt() {
                            break;
                        }
                        if start.elapsed() >= Duration::from_millis(100) {
                            return Err(ArmError::Timeout);
                        }
                    }
                }
                return Ok(());
            }
        }

        Err(ArmError::Timeout)
    }

    fn debug_core_start(
        &self,
        interface: &mut dyn ArmProbeInterface,
        core_ap: MemoryAp,
        _core_type: CoreType,
        _debug_base: Option<u64>,
        _cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut core = interface.memory_interface(core_ap)?;

        self.release_reset_extension(&mut *core)?;

        self.exit_boot_rom_to_app(&mut *core)?;

        Ok(())
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

        self.release_reset_extension(memory)?;

        self.exit_boot_rom_to_app(memory)?;

        Ok(())
    }
}
