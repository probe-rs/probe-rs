use crate::DebugProbeError;

use super::DebugProbe;
use blackmagicprobe::BlackMagicProbe;

#[derive(Debug)]
pub(crate) struct Bmp {
    handle: BlackMagicProbe,
}

impl DebugProbe for Bmp {
    /// Creates a new boxed [`DebugProbe`] from a given [`DebugProbeSelector`].
    /// This will be called for all available debug drivers when discovering probes.
    /// When opening, it will open the first probe which succeeds during this call.
    // fn new_from_selector(
    //     selector: impl Into<DebugProbeSelector>,
    // ) -> Result<Box<Self>, DebugProbeError>
    // where
    //     Self: Sized;

    /// Get human readable name for the probe.
    fn get_name(&self) -> &str {
        "Black Magic Probe"
    }

    fn speed_khz(&self) -> u32 {
        return self.handle.max_speed_get() / 1000;
    }

    /// Set the speed in kHz used for communication with the target device.
    ///
    /// The desired speed might not be supported by the probe. If the desired
    /// speed is not directly supported, a lower speed will be selected if possible.
    ///
    /// If possible, the actual speed used is returned by the function. Some probes
    /// cannot report this, so the value may be inaccurate.
    ///
    /// If the requested speed is not supported,
    /// `DebugProbeError::UnsupportedSpeed` will be returned.
    ///
    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
       self.handle.max_speed_set(speed_khz * 1000);
        return Ok(self.speed_khz());
    }

    /// Attach to the chip.
    ///
    /// This should run all the necessary protocol init routines.
    // fn attach(&mut self) -> Result<(), DebugProbeError>;

    /// Detach from the chip.
    ///
    /// This should run all the necessary protocol deinit routines.
    ///
    /// If the probe uses batched commands, this will also cause all
    /// remaining commands to be executed. If an error occurs during
    /// this execution, the probe might remain in the attached state.
    // fn detach(&mut self) -> Result<(), crate::Error>;

    /// This should hard reset the target device.
    // fn target_reset(&mut self) -> Result<(), DebugProbeError>;

    /// This should assert the reset pin of the target via debug probe.
    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {

    }

    /// This should deassert the reset pin of the target via debug probe.
    // fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {

    // }

    /// Selects the transport protocol to be used by the debug probe.
    // fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError>;

    /// Get the transport protocol currently in active use by the debug probe.
    // fn active_protocol(&self) -> Option<WireProtocol>;

    /// Check if the proble offers an interface to debug ARM chips.
    // fn has_arm_interface(&self) -> bool {
    //     false
    // }

    /// Get the dedicated interface to debug ARM chips. To check that the
    /// probe actually supports this, call [DebugProbe::has_arm_interface] first.
    // fn try_get_arm_interface<'probe>(
    //     self: Box<Self>,
    // ) -> Result<Box<dyn UninitializedArmProbe + 'probe>, (Box<dyn DebugProbe>, DebugProbeError)>
    // {
    //     Err((
    //         self.into_probe(),
    //         DebugProbeError::InterfaceNotAvailable("ARM"),
    //     ))
    // }

    /// Get the dedicated interface to debug RISCV chips. Ensure that the
    /// probe actually supports this by calling [DebugProbe::has_riscv_interface] first.
    // fn try_get_riscv_interface(
    //     self: Box<Self>,
    // ) -> Result<RiscvCommunicationInterface, (Box<dyn DebugProbe>, RiscvError)> {
    //     Err((
    //         self.into_probe(),
    //         DebugProbeError::InterfaceNotAvailable("RISCV").into(),
    //     ))
    // }

    /// Check if the probe offers an interface to debug RISCV chips.
    // fn has_riscv_interface(&self) -> bool {
    //     false
    // }

    /// Get a SWO interface from the debug probe.
    ///
    /// This is not available on all debug probes.
    // fn get_swo_interface(&self) -> Option<&dyn SwoAccess> {
    //     None
    // }

    /// Get a mutable SWO interface from the debug probe.
    ///
    /// This is not available on all debug probes.
    // fn get_swo_interface_mut(&mut self) -> Option<&mut dyn SwoAccess> {
    //     None
    // }

    /// Boxes itself.
    // fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe>;

    /// Try creating a DAP interface for the given probe.
    ///
    /// This is not available on all probes.
    // fn try_as_dap_probe(&mut self) -> Option<&mut dyn DapProbe> {
    //     None
    // }

    /// Reads the target voltage in Volts, if possible. Returns `Ok(None)`
    /// if the probe doesn’t support reading the target voltage.
    // fn get_target_voltage(&mut self) -> Result<Option<f32>, DebugProbeError> {
    //     Ok(None)
    // }
}
