use std::{fmt::Debug, sync::Arc};

use crate::{
    architecture::arm::{
        ap::{memory_ap::mock::MockMemoryAp, AccessPort, MemoryAp},
        communication_interface::{
            ArmDebugState, Initialized, SwdSequence, Uninitialized, UninitializedArmProbe,
        },
        dp::DebugPortError,
        memory::adi_v5_memory_interface::{ADIMemoryInterface, ArmProbe},
        sequences::ArmDebugSequence,
        ApAddress, ArmError, ArmProbeInterface, DapAccess, DpAddress, MemoryApInformation,
        PortType, RawDapAccess, SwoAccess,
    },
    DebugProbe, DebugProbeError, DebugProbeSelector, Error, Probe, WireProtocol,
};

/// This is a mock probe which can be used for mocking things in tests or for dry runs.
#[allow(clippy::type_complexity)]
pub struct FakeProbe {
    protocol: WireProtocol,
    speed: u32,

    dap_register_read_handler: Option<Box<dyn Fn(PortType, u8) -> Result<u32, ArmError> + Send>>,

    dap_register_write_handler:
        Option<Box<dyn Fn(PortType, u8, u32) -> Result<(), ArmError> + Send>>,
}

impl Debug for FakeProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeProbe")
            .field("protocol", &self.protocol)
            .field("speed", &self.speed)
            .finish()
    }
}

impl FakeProbe {
    /// Creates a new [`FakeProbe`] for mocking.
    pub fn new() -> Self {
        FakeProbe {
            protocol: WireProtocol::Swd,
            speed: 1000,

            dap_register_read_handler: None,
            dap_register_write_handler: None,
        }
    }

    /// This sets the read handler for DAP register reads.
    /// Can be used to hook into the read.
    pub fn set_dap_register_read_handler(
        &mut self,
        handler: Box<dyn Fn(PortType, u8) -> Result<u32, ArmError> + Send>,
    ) {
        self.dap_register_read_handler = Some(handler);
    }

    /// This sets the write handler for DAP register writes.
    /// Can be used to hook into the write.
    pub fn set_dap_register_write_handler(
        &mut self,
        handler: Box<dyn Fn(PortType, u8, u32) -> Result<(), ArmError> + Send>,
    ) {
        self.dap_register_write_handler = Some(handler);
    }

    /// Makes a generic probe out of the [`FakeProbe`]
    pub fn into_probe(self) -> Probe {
        Probe::from_specific_probe(Box::new(self))
    }
}

impl Default for FakeProbe {
    fn default() -> Self {
        FakeProbe::new()
    }
}

impl DebugProbe for FakeProbe {
    fn new_from_selector(
        _selector: impl Into<DebugProbeSelector>,
    ) -> Result<Box<Self>, DebugProbeError>
    where
        Self: Sized,
    {
        Ok(Box::new(FakeProbe::new()))
    }

    /// Get human readable name for the probe
    fn get_name(&self) -> &str {
        "Mock probe for testing"
    }

    fn speed_khz(&self) -> u32 {
        self.speed
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        self.speed = speed_khz;

        Ok(speed_khz)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        self.protocol = protocol;

        Ok(())
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        Some(self.protocol)
    }

    /// Leave debug mode
    fn detach(&mut self) -> Result<(), crate::Error> {
        Ok(())
    }

    /// Resets the target device.
    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::CommandNotSupportedByProbe("target_reset"))
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        unimplemented!()
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        unimplemented!()
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn try_get_arm_interface<'probe>(
        self: Box<Self>,
    ) -> Result<Box<dyn UninitializedArmProbe + 'probe>, (Box<dyn DebugProbe>, DebugProbeError)>
    {
        Ok(Box::new(FakeArmInterface::new(self)))
    }

    fn has_arm_interface(&self) -> bool {
        true
    }
}

impl RawDapAccess for FakeProbe {
    fn select_dp(&mut self, _dp: DpAddress) -> Result<(), ArmError> {
        Err(DebugPortError::Unsupported(
            "Fake debug probe does not support DP selection.".to_string(),
        )
        .into())
    }

    /// Reads the DAP register on the specified port and address
    fn raw_read_register(&mut self, port: PortType, addr: u8) -> Result<u32, ArmError> {
        let handler = self.dap_register_read_handler.as_ref().unwrap();

        handler(port, addr)
    }

    /// Writes a value to the DAP register on the specified port and address
    fn raw_write_register(&mut self, port: PortType, addr: u8, value: u32) -> Result<(), ArmError> {
        let handler = self.dap_register_write_handler.as_ref().unwrap();

        handler(port, addr, value)
    }

    fn swj_sequence(&mut self, _bit_len: u8, _bits: u64) -> Result<(), DebugProbeError> {
        todo!()
    }

    fn swj_pins(
        &mut self,
        _pin_out: u32,
        _pin_select: u32,
        _pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        todo!()
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }
}

#[derive(Debug)]
struct FakeArmInterface<S: ArmDebugState> {
    probe: Box<FakeProbe>,

    memory_ap: MockMemoryAp,

    _state: S,
}

impl FakeArmInterface<Uninitialized> {
    pub(crate) fn new(probe: Box<FakeProbe>) -> Self {
        let state = Uninitialized {
            use_overrun_detect: false,
        };
        let memory_ap = MockMemoryAp::with_pattern();

        Self {
            probe,
            _state: state,
            memory_ap,
        }
    }

    fn into_initialized(
        self,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<FakeArmInterface<Initialized>, (Box<Self>, DebugProbeError)> {
        Ok(FakeArmInterface::<Initialized>::from_uninitialized(
            self, sequence,
        ))
    }
}

impl FakeArmInterface<Initialized> {
    fn from_uninitialized(
        interface: FakeArmInterface<Uninitialized>,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Self {
        let memory_ap = MockMemoryAp::with_pattern();
        FakeArmInterface::<Initialized> {
            probe: interface.probe,
            _state: Initialized::new(sequence, false),
            memory_ap,
        }
    }
}

impl<S: ArmDebugState> SwdSequence for FakeArmInterface<S> {
    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError> {
        self.probe.swj_sequence(bit_len, bits)?;

        Ok(())
    }

    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        let value = self.probe.swj_pins(pin_out, pin_select, pin_wait)?;

        Ok(value)
    }
}

impl UninitializedArmProbe for FakeArmInterface<Uninitialized> {
    fn initialize(
        self: Box<Self>,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Box<dyn ArmProbeInterface>, (Box<dyn UninitializedArmProbe>, Error)> {
        // TODO: Do we need this?
        // sequence.debug_port_setup(&mut self.probe)?;

        let interface = self
            .into_initialized(sequence)
            .map_err(|(s, err)| (s as Box<_>, Error::Probe(err)))?;

        Ok(Box::new(interface))
    }

    fn close(self: Box<Self>) -> Probe {
        Probe::from_attached_probe(self.probe)
    }
}

impl ArmProbeInterface for FakeArmInterface<Initialized> {
    fn memory_interface(
        &mut self,
        access_port: MemoryAp,
    ) -> Result<Box<dyn ArmProbe + '_>, ArmError> {
        let ap_information = MemoryApInformation {
            address: access_port.ap_address(),
            supports_only_32bit_data_size: false,
            debug_base_address: 0xf000_0000,
            supports_hnonsec: false,
            has_large_data_extension: false,
            has_large_address_extension: false,
            device_enabled: true,
        };

        let memory = ADIMemoryInterface::new(&mut self.memory_ap, ap_information)
            .map_err(|e| ArmError::from_access_port(e, access_port))?;

        Ok(Box::new(memory) as _)
    }

    fn ap_information(
        &mut self,
        _access_port: crate::architecture::arm::ap::GenericAp,
    ) -> Result<&crate::architecture::arm::ApInformation, ArmError> {
        todo!()
    }

    fn num_access_ports(&mut self, _dp: DpAddress) -> Result<usize, ArmError> {
        Ok(1)
    }

    fn read_chip_info_from_rom_table(
        &mut self,
        _dp: DpAddress,
    ) -> Result<Option<crate::architecture::arm::ArmChipInfo>, ArmError> {
        Ok(None)
    }

    fn close(self: Box<Self>) -> Probe {
        Probe::from_attached_probe(self.probe)
    }
}

impl SwoAccess for FakeArmInterface<Initialized> {
    fn enable_swo(
        &mut self,
        _config: &crate::architecture::arm::SwoConfig,
    ) -> Result<(), ArmError> {
        unimplemented!()
    }

    fn disable_swo(&mut self) -> Result<(), ArmError> {
        unimplemented!()
    }

    fn read_swo_timeout(&mut self, _timeout: std::time::Duration) -> Result<Vec<u8>, ArmError> {
        unimplemented!()
    }
}

impl DapAccess for FakeArmInterface<Initialized> {
    fn read_raw_dp_register(&mut self, _dp: DpAddress, _address: u8) -> Result<u32, ArmError> {
        todo!()
    }

    fn write_raw_dp_register(
        &mut self,
        _dp: DpAddress,
        _address: u8,
        _value: u32,
    ) -> Result<(), ArmError> {
        todo!()
    }

    fn read_raw_ap_register(&mut self, _ap: ApAddress, _address: u8) -> Result<u32, ArmError> {
        todo!()
    }

    fn read_raw_ap_register_repeated(
        &mut self,
        _ap: ApAddress,
        _address: u8,
        _values: &mut [u32],
    ) -> Result<(), ArmError> {
        todo!()
    }

    fn write_raw_ap_register(
        &mut self,
        _ap: ApAddress,
        _address: u8,
        _value: u32,
    ) -> Result<(), ArmError> {
        todo!()
    }

    fn write_raw_ap_register_repeated(
        &mut self,
        _ap: ApAddress,
        _address: u8,
        _values: &[u32],
    ) -> Result<(), ArmError> {
        todo!()
    }
}

#[cfg(test)]
mod test {
    use super::FakeProbe;
    use crate::Permissions;

    #[test]
    fn create_session_with_fake_probe() {
        let fake_probe = FakeProbe::new();

        let probe = fake_probe.into_probe();

        probe
            .attach("nrf51822_xxAC", Permissions::default())
            .unwrap();
    }
}
