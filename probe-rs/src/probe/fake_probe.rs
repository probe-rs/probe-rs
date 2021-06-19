use std::fmt::Debug;

use crate::{
    architecture::arm::{
        ap::{memory_ap::mock::MockMemoryAp, AccessPort, MemoryAp},
        memory::adi_v5_memory_interface::ADIMemoryInterface,
        ArmProbeInterface, DapAccess, MemoryApInformation, PortType, SwoAccess,
    },
    DebugProbe, DebugProbeError, DebugProbeSelector, Error, Memory, Probe, WireProtocol,
};

pub struct FakeProbe {
    protocol: WireProtocol,
    speed: u32,

    dap_register_read_handler:
        Option<Box<dyn Fn(PortType, u16) -> Result<u32, DebugProbeError> + Send>>,

    dap_register_write_handler:
        Option<Box<dyn Fn(PortType, u16, u32) -> Result<(), DebugProbeError> + Send>>,
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
    pub fn new() -> Self {
        FakeProbe {
            protocol: WireProtocol::Swd,
            speed: 1000,

            dap_register_read_handler: None,
            dap_register_write_handler: None,
        }
    }

    pub fn handle_dap_register_read(
        &mut self,
        handler: Box<dyn Fn(PortType, u16) -> Result<u32, DebugProbeError> + Send>,
    ) {
        self.dap_register_read_handler = Some(handler);
    }

    pub fn handle_dap_register_write(
        &mut self,
        handler: Box<dyn Fn(PortType, u16, u32) -> Result<(), DebugProbeError> + Send>,
    ) {
        self.dap_register_write_handler = Some(handler);
    }

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

    fn speed(&self) -> u32 {
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

    /// Leave debug mode
    fn detach(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }

    /// Resets the target device.
    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::CommandNotSupportedByProbe)
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
    ) -> Result<Box<dyn ArmProbeInterface + 'probe>, (Box<dyn DebugProbe>, DebugProbeError)> {
        Ok(Box::new(FakeArmInterface::new(self)))
    }

    fn has_arm_interface(&self) -> bool {
        true
    }
}

impl DapAccess for FakeProbe {
    /// Reads the DAP register on the specified port and address
    fn read_register(&mut self, port: PortType, addr: u16) -> Result<u32, DebugProbeError> {
        if let Some(handler) = &self.dap_register_read_handler {
            handler(port, addr)
        } else {
            Err(DebugProbeError::CommandNotSupportedByProbe)
        }
    }

    /// Writes a value to the DAP register on the specified port and address
    fn write_register(
        &mut self,
        port: PortType,
        addr: u16,
        value: u32,
    ) -> Result<(), DebugProbeError> {
        if let Some(handler) = &self.dap_register_write_handler {
            handler(port, addr, value)
        } else {
            Err(DebugProbeError::CommandNotSupportedByProbe)
        }
    }
}

#[derive(Debug)]
struct FakeArmInterface {
    probe: Box<FakeProbe>,

    memory_ap: MockMemoryAp,
}

impl FakeArmInterface {
    fn new(probe: Box<FakeProbe>) -> Self {
        let memory_ap = MockMemoryAp::with_pattern();
        FakeArmInterface { probe, memory_ap }
    }
}

impl ArmProbeInterface for FakeArmInterface {
    fn memory_interface(&mut self, access_port: MemoryAp) -> Result<Memory<'_>, Error> {
        let ap_information = MemoryApInformation {
            port_number: access_port.port_number(),
            only_32bit_data_size: false,
            debug_base_address: 0xf000_0000,
            supports_hnonsec: false,
        };

        let memory = ADIMemoryInterface::new(&mut self.memory_ap, &ap_information)?;

        Ok(Memory::new(memory, access_port))
    }

    fn ap_information(
        &self,
        _access_port: crate::architecture::arm::ap::GenericAp,
    ) -> Option<&crate::architecture::arm::ApInformation> {
        todo!()
    }

    fn num_access_ports(&self) -> usize {
        1
    }

    fn read_from_rom_table(
        &mut self,
    ) -> Result<Option<crate::architecture::arm::ArmChipInfo>, Error> {
        Ok(None)
    }

    fn close(self: Box<Self>) -> Probe {
        Probe::from_attached_probe(self.probe)
    }

    fn target_reset_deassert(&mut self) -> Result<(), Error> {
        todo!()
    }
}

impl SwoAccess for FakeArmInterface {
    fn enable_swo(&mut self, _config: &crate::architecture::arm::SwoConfig) -> Result<(), Error> {
        unimplemented!()
    }

    fn disable_swo(&mut self) -> Result<(), Error> {
        unimplemented!()
    }

    fn read_swo_timeout(&mut self, _timeout: std::time::Duration) -> Result<Vec<u8>, Error> {
        unimplemented!()
    }
}

#[cfg(test)]
mod test {
    use super::FakeProbe;

    #[test]
    fn create_session_with_fake_probe() {
        let mut fake_probe = FakeProbe::new();

        let probe = fake_probe.into_probe();

        probe.attach("nrf51822").unwrap();
    }
}
