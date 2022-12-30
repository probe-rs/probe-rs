//! Sequences for ATSAM D5x/E5x target families

use super::{ArmDebugSequence, ArmDebugSequenceError, DebugEraseSequence};
use crate::{
    architecture::{
        self,
        arm::{
            ap::MemoryAp, memory::adi_v5_memory_interface::ArmProbe, ApAddress, ArmError,
            ArmProbeInterface, DpAddress,
        },
    },
    session::MissingPermissions,
    DebugProbeError, Permissions,
};
use bitfield::bitfield;
use std::sync::Arc;

use anyhow::Result;

bitfield! {
    /// Device Service Unit Control Register, DSU - CTRL
    #[derive(Copy, Clone)]
    pub struct DsuCtrl(u8);
    impl Debug;
    /// Chip-Erase
    /// Writing a '0' to this bit has no effect.
    /// Writing a '1' to this bit starts the Chip-Erase operation.
    pub _, set_ce: 4;
    /// Memory Built-In Self-Test
    /// Writing a '0' to this bit has no effect.
    /// Writing a '1' to this bit starts the memory BIST algorithm.
    pub _, set_mbist: 3;
    /// 32-bit Cyclic Redundancy Check
    /// Writing a '0' to this bit has no effect.
    /// Writing a '1' to this bit starts the cyclic redundancy check algorithm.
    pub _, set_crc: 2;
    /// Software Reset
    /// Writing a '0' to this bit has no effect.
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
    /// Device Service Unit Control Register, DSU - CTRL
    #[derive(Copy, Clone)]
    pub struct DsuStatusA(u8);
    impl Debug;
    /// Protection Error
    /// Writing a '0' to this bit has no effect.
    /// Writing a '1' to this bit clears the Protection Error bit.
    /// This bit is set when a command that is not allowed in Protected state is issued.
    pub perr, set_perr: 4;

    /// Failure
    /// Writing a '0' to this bit has no effect.
    /// Writing a '1' to this bit clears the Failure bit.
    /// This bit is set when a DSU operation failure is detected.
    pub fail, set_fail: 3;

    /// Bus Error
    /// Writing a '0' to this bit has no effect.
    /// Writing a '1' to this bit clears the Bus Error bit.
    /// This bit is set when a bus error is detected.
    pub berr, set_berr: 2;

    /// CPU Reset Phase Extension
    /// Writing a '0' to this bit has no effect.
    /// Writing a '1' to this bit clears the CPU Reset Phase Extension bit.
    /// This bit is set when a debug adapter Cold-Plugging is detected, which extends the CPU Reset phase.
    pub crstext, set_crstext: 1;

    /// Done
    /// Writing a '0' to this bit has no effect.
    /// Writing a '1' to this bit clears the Done bit.
    /// This bit is set when a DSU operation is completed.
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
    /// Device Service Unit Control Register, DSU - CTRL
    #[derive(Copy, Clone)]
    pub struct DsuStatusB(u8);
    impl Debug;

    /// Chip Erase Locked
    /// This bit is set when Chip Erase is locked.
    /// This bit is cleared when Chip Erase is unlocked.
    pub celck, _: 5;
    /// Hot-Plugging Enable
    /// This bit is set when Hot-Plugging is enabled.
    /// This bit is cleared when Hot-Plugging is disabled. This is the case when the SWCLK function is changed.
    /// Only a power-reset or a external reset can set it again.
    pub hpe, _: 4;
    /// Debug Communication Channel 1 Dirty
    /// This bit is set when DCC is written.
    /// This bit is cleared when DCC is read.
    pub dccd1, _: 3;
    /// Debug Communication Channel 0 Dirty
    /// This bit is set when DCC is written.
    /// This bit is cleared when DCC is read.
    pub dccd0, _: 2;
    /// Debugger Present
    /// This bit is set when a debugger probe is detected.
    /// This bit is never cleared.
    pub dbgpres, _: 1;
    /// Protected
    /// This bit is set at power-up when the device is protected.
    /// This bit is never cleared.
    pub prot, _: 0;

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
struct SwdSequenceShim<'a>(&'a mut dyn architecture::arm::communication_interface::DapProbe);

impl<'a> From<&'a mut dyn architecture::arm::communication_interface::DapProbe>
    for SwdSequenceShim<'a>
{
    fn from(p: &'a mut dyn architecture::arm::communication_interface::DapProbe) -> Self {
        Self(p)
    }
}

impl<'a> architecture::arm::communication_interface::SwdSequence for SwdSequenceShim<'a> {
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

/// Marker struct indicating initialization sequencing for ATSAM D5x/E5x family parts.
pub struct AtSAME5x {}

impl AtSAME5x {
    /// Create the sequencer for the ATSAM D5x/E5x family of parts.
    pub fn create() -> Arc<Self> {
        Arc::new(Self {})
    }

    /// Perform a Chip-Erase operation
    ///
    /// Issue a Chip-Erase command to the device provided that `permission` grants `erase-all`.
    ///
    /// # Errors
    /// This operation can fail due to insufficient permissions, or if Chip-Erase Lock is
    /// enabled (this lock can only be released from within the device firmare).
    /// After a successful Chip-Erase a `DebugProbeError::ReAttachRequired` is returned
    /// to signal that a re-connect is needed for the DSU to start operating in unlocked mode.
    pub fn erase_all(
        &self,
        memory: &mut dyn ArmProbe,
        permissions: &Permissions,
    ) -> Result<(), ArmError> {
        let dsu_status_a = DsuStatusA::from(memory.read_word_8(DsuStatusA::ADDRESS)?);
        let dsu_status_b = DsuStatusB::from(memory.read_word_8(DsuStatusB::ADDRESS)?);

        match (dsu_status_b.celck(), dsu_status_b.prot(), permissions.erase_all()) {
            (true, _, _) => Err(ArmError::MissingPermissions(
                "Chip-Erase is locked. This can only be unlocked from within the device firmware by performing \
                a Chip-Erase Unlock (CEULCK) command."
                    .into(),
            )),
            (false, true, Err(MissingPermissions(permission))) => Err(ArmError::MissingPermissions(
                format!("Device is locked. A Chip-Erase operation is required to unlock. \
                            Re-run with granting the '{}' permission and connecting under reset"
                    , permission),
            )),
            // TODO: This seems wrong? Currently preserves the bevaiour before the change of the error type.
            (false, false,Err(MissingPermissions(permission))) => Err(ArmError::MissingPermissions(permission)),
            (false, _, Ok(())) => Ok(()),
        }?;

        // Start the chip-erase process
        let mut dsu_ctrl = DsuCtrl(0);
        dsu_ctrl.set_ce(true);
        memory.write_word_8(DsuCtrl::ADDRESS, dsu_ctrl.0)?;
        tracing::info!("Chip-Erase started..");

        // Wait for it to finish
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(8) {
            let current_dsu_statusa = DsuStatusA::from(memory.read_word_8(DsuStatusA::ADDRESS)?);
            if current_dsu_statusa.done() {
                tracing::info!("Chip-Erase complete");
                // If the device was in Reset Extension when we started put it back into Reset Extension
                if dsu_status_a.crstext() {
                    self.reset_hardware_with_extension(memory.get_arm_communication_interface()?)?;
                } else {
                    self.reset_hardware(memory.get_arm_communication_interface()?)?;
                }

                // We need to reconnect to target to finalize the unlock.
                // Signal ReAttachRequired so that the session will try to re-connect
                return Err(ArmError::ReAttachRequired);
            } else if current_dsu_statusa.fail() {
                return Err(ArmError::ChipEraseFailed);
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }

        tracing::error!("Chip-Erase failed to complete within 8 seconds");
        Err(ArmError::Timeout)
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
        interface: &mut dyn architecture::arm::communication_interface::SwdSequence,
    ) -> Result<(), ArmError> {
        let mut pins = architecture::arm::Pins(0);
        pins.set_nreset(true);
        pins.set_swdio_tms(true);
        pins.set_swclk_tck(true);

        let mut pin_values = architecture::arm::Pins(0);

        // First set nReset, SWDIO and SWCLK to low.
        let _ = interface.swj_pins(pin_values.0 as u32, pins.0 as u32, 0)?;
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Next release nReset, but keep SWDIO and SWCLK low. This should put the device into
        // reset extension ()
        pin_values.set_nreset(true);
        let _ = interface.swj_pins(pin_values.0 as u32, pins.0 as u32, 0)?;
        std::thread::sleep(std::time::Duration::from_millis(20));

        Ok(())
    }

    /// Release the CPU core from Reset Extension
    ///
    /// Clear the DSU Reset Extension bit, which releases the core from reset extension.
    ///
    /// # Errors
    /// Subject to probe communication errors
    pub fn release_reset_extension(&self, memory: &mut dyn ArmProbe) -> Result<(), ArmError> {
        // clear the reset extension bit
        let mut dsu_statusa = DsuStatusA(0);
        dsu_statusa.set_crstext(true);
        memory.write_8(DsuStatusA::ADDRESS, &[dsu_statusa.0])?;

        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(100) {
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
    pub fn reset_hardware(
        &self,
        interface: &mut dyn architecture::arm::communication_interface::SwdSequence,
    ) -> Result<(), ArmError> {
        let mut pins = architecture::arm::Pins(0);
        pins.set_nreset(true);
        pins.set_swdio_tms(true);
        pins.set_swclk_tck(true);

        let mut pin_values = pins;
        pin_values.set_nreset(false);

        let _ = interface.swj_pins(pin_values.0 as u32, pins.0 as u32, 0)?;
        std::thread::sleep(std::time::Duration::from_millis(10));

        pin_values.set_nreset(true);
        let _ = interface.swj_pins(pin_values.0 as u32, pins.0 as u32, 0)?;
        std::thread::sleep(std::time::Duration::from_millis(10));

        Ok(())
    }
}

impl ArmDebugSequence for AtSAME5x {
    /// `reset_hardware_assert` for ATSAM D5x/E5x devices
    ///
    /// Instead of keeping `nReset` asserted, the device is instead put into CPU Reset Extension
    /// which will keep the CPU Core in reset until manually released by the debugger probe.
    fn reset_hardware_assert(
        &self,
        interface: &mut dyn architecture::arm::communication_interface::DapProbe,
    ) -> Result<(), ArmError> {
        let mut shim = SwdSequenceShim::from(interface);
        self.reset_hardware_with_extension(&mut shim)
    }

    /// `reset_hardware_deassert` for ATSAM D5x/E5x devices
    ///
    /// Instead of de-asserting `nReset` here (this was already done during the CPU Reset Extension process),
    /// the device is released from Reset Extension.
    fn reset_hardware_deassert(&self, memory: &mut dyn ArmProbe) -> Result<(), ArmError> {
        let mut pins = architecture::arm::Pins(0);
        pins.set_nreset(true);

        let current_pins =
            architecture::arm::Pins(memory.swj_pins(pins.0 as u32, pins.0 as u32, 0)? as u8);
        if !current_pins.nreset() {
            return Err(ArmDebugSequenceError::SequenceSpecific(
                "Expected nReset to already be de-asserted".into(),
            )
            .into());
        }

        self.release_reset_extension(memory)
    }

    /// `debug_device_unlock` for ATSAM D5x/E5x devices
    ///
    /// First check the device lock status by querying its Device Service Unit (DSU).
    /// If the device is already unlocked then return `Ok` directly.
    /// If the device is locked the following happens:
    /// * If the `erase_all` permission is missing return the appropriate error
    /// * If the Chip-Erase command is also locked then return an error since Chip-Erase Unlock can only be
    ///   done from within the device firmware.
    /// * Perform a Chip-Erase to unlock the device and if successful return a `DebugProbeError::ReAttachRequired`
    ///   to signal that a probe re-attach is required before the new `unlocked` status takes effect.
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmProbeInterface,
        default_ap: architecture::arm::ap::MemoryAp,
        permissions: &Permissions,
    ) -> Result<(), ArmError> {
        // First check if the device is locked
        let mut memory = interface.memory_interface(default_ap)?;
        let dsu_status_b = DsuStatusB::from(memory.read_word_8(DsuStatusB::ADDRESS)?);

        if dsu_status_b.prot() {
            tracing::warn!("The Device is locked, unlocking..");
            self.erase_all(&mut *memory, permissions)
        } else {
            Ok(())
        }
    }

    fn debug_erase_sequence(&self) -> Option<Arc<dyn DebugEraseSequence>> {
        Some(Self::create())
    }
}

impl DebugEraseSequence for AtSAME5x {
    fn erase_all(&self, interface: &mut dyn ArmProbeInterface) -> Result<(), ArmError> {
        let mem_ap = MemoryAp::new(ApAddress {
            dp: DpAddress::Default,
            ap: 0,
        });

        let mut memory = interface.memory_interface(mem_ap)?;

        AtSAME5x::erase_all(self, &mut *memory, &Permissions::new().allow_erase_all())
    }
}
