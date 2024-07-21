#![allow(missing_docs)] // Don't require docs for test code
use std::{cell::RefCell, collections::VecDeque, fmt::Debug, sync::Arc};

use probe_rs_target::ScanChainElement;

use crate::{
    architecture::arm::{
        ap::{memory_ap::mock::MockMemoryAp, AccessPort, MemoryAp},
        armv8m::Dhcsr,
        communication_interface::{
            ArmDebugState, Initialized, SwdSequence, Uninitialized, UninitializedArmProbe,
        },
        memory::adi_v5_memory_interface::{ADIMemoryInterface, ArmMemoryInterface},
        sequences::ArmDebugSequence,
        ArmError, ArmProbeInterface, DapAccess, DpAddress, FullyQualifiedApAddress,
        MemoryApInformation, PortType, RawDapAccess, SwoAccess,
    },
    probe::{DebugProbe, DebugProbeError, Probe, WireProtocol},
    Error, MemoryMappedRegister,
};

/// This is a mock probe which can be used for mocking things in tests or for dry runs.
#[allow(clippy::type_complexity)]
pub struct FakeProbe {
    protocol: WireProtocol,
    speed: u32,
    scan_chain: Option<Vec<ScanChainElement>>,

    dap_register_read_handler: Option<Box<dyn Fn(PortType, u8) -> Result<u32, ArmError> + Send>>,

    dap_register_write_handler:
        Option<Box<dyn Fn(PortType, u8, u32) -> Result<(), ArmError> + Send>>,

    operations: RefCell<VecDeque<Operation>>,

    memory_ap: MockedAp,
}

enum MockedAp {
    /// Mock a memory AP
    MemoryAp(MockMemoryAp),
    /// Mock an ARM core behind a memory AP
    Core(MockCore),
}

struct MockCore {
    dhcsr: Dhcsr,

    /// Is the core halted?
    is_halted: bool,
}

impl MockCore {
    pub fn new() -> Self {
        Self {
            dhcsr: Dhcsr(0),
            is_halted: false,
        }
    }
}

impl SwdSequence for &mut MockCore {
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
}

impl ArmMemoryInterface for &mut MockCore {
    fn read_8(&mut self, _address: u64, _data: &mut [u8]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_16(&mut self, _address: u64, _data: &mut [u16]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError> {
        for (i, val) in data.iter_mut().enumerate() {
            let address = address + (i as u64 * 4);

            match address {
                // DHCSR
                Dhcsr::ADDRESS_OFFSET => {
                    let mut dhcsr: u32 = self.dhcsr.into();

                    if self.is_halted {
                        dhcsr |= 1 << 17;
                    }

                    // Always set S_REGRDY, and say that a register value can
                    // be read.
                    dhcsr |= 1 << 16;

                    *val = dhcsr;
                    println!("Read  DHCSR: {:#x} = {:#x}", address, val);
                }

                _ => {
                    *val = 0;
                    println!("Read {:#010x} = 0", address);
                }
            }
        }

        Ok(())
    }

    fn read_64(&mut self, _address: u64, _data: &mut [u64]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_8(&mut self, _address: u64, _data: &[u8]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_16(&mut self, _address: u64, _data: &[u16]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), ArmError> {
        for (i, word) in data.iter().enumerate() {
            let address = address + (i as u64 * 4);

            match address {
                // DHCSR
                Dhcsr::ADDRESS_OFFSET => {
                    let dbg_key = (*word >> 16) & 0xffff;

                    if dbg_key == 0xa05f {
                        // Mask out dbg key
                        self.dhcsr = Dhcsr::from(*word & 0xffff);
                        println!("Write DHCSR = {:#010x}", word);

                        let request_halt = self.dhcsr.c_halt();

                        self.is_halted = request_halt;

                        if !self.dhcsr.c_halt() && self.dhcsr.c_debugen() && self.dhcsr.c_step() {
                            tracing::debug!("MockCore: Single step requested, setting s_halt");
                            self.is_halted = true;
                        }
                    }
                }
                _ => println!("Write {:#010x} = {:#010x}", address, word),
            }
        }

        Ok(())
    }

    fn write_64(&mut self, _address: u64, _data: &[u64]) -> Result<(), ArmError> {
        todo!()
    }

    fn flush(&mut self) -> Result<(), ArmError> {
        Ok(())
    }

    fn supports_native_64bit_access(&mut self) -> bool {
        true
    }

    fn supports_8bit_transfers(&self) -> Result<bool, ArmError> {
        todo!()
    }

    fn ap(&mut self) -> MemoryAp {
        todo!()
    }

    fn get_arm_communication_interface(
        &mut self,
    ) -> Result<
        &mut crate::architecture::arm::ArmCommunicationInterface<Initialized>,
        DebugProbeError,
    > {
        todo!()
    }

    fn update_core_status(&mut self, _state: crate::CoreStatus) {}
}

#[derive(Debug, Clone, PartialEq)]
pub enum Operation {
    ReadRawApRegister {
        ap: FullyQualifiedApAddress,
        address: u8,
        result: u32,
    },
}

impl Debug for FakeProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeProbe")
            .field("protocol", &self.protocol)
            .field("speed", &self.speed)
            .finish_non_exhaustive()
    }
}

impl FakeProbe {
    /// Creates a new [`FakeProbe`] for mocking.
    pub fn new() -> Self {
        FakeProbe {
            protocol: WireProtocol::Swd,
            speed: 1000,
            scan_chain: None,

            dap_register_read_handler: None,
            dap_register_write_handler: None,

            operations: RefCell::new(VecDeque::new()),

            memory_ap: MockedAp::MemoryAp(MockMemoryAp::with_pattern()),
        }
    }

    /// Fake probe with a mocked core
    pub fn with_mocked_core() -> Self {
        FakeProbe {
            protocol: WireProtocol::Swd,
            speed: 1000,
            scan_chain: None,

            dap_register_read_handler: None,
            dap_register_write_handler: None,

            operations: RefCell::new(VecDeque::new()),

            memory_ap: MockedAp::Core(MockCore::new()),
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

    fn next_operation(&self) -> Option<Operation> {
        self.operations.borrow_mut().pop_front()
    }

    fn read_raw_ap_register(
        &mut self,
        expected_ap: &FullyQualifiedApAddress,
        expected_address: u8,
    ) -> Result<u32, ArmError> {
        let operation = self.next_operation();

        match operation {
            Some(Operation::ReadRawApRegister {
                ap,
                address,
                result,
            }) => {
                assert_eq!(&ap, expected_ap);
                assert_eq!(address, expected_address);

                Ok(result)
            }
            None => panic!("No more operations expected, but got read_raw_ap_register ap={expected_ap:?}, address:{expected_address}"),
            //other => panic!("Unexpected operation: {:?}", other),
        }
    }

    pub fn expect_operation(&self, operation: Operation) {
        self.operations.borrow_mut().push_back(operation);
    }
}

impl Default for FakeProbe {
    fn default() -> Self {
        FakeProbe::new()
    }
}

impl DebugProbe for FakeProbe {
    /// Get human readable name for the probe
    fn get_name(&self) -> &str {
        "Mock probe for testing"
    }

    fn speed_khz(&self) -> u32 {
        self.speed
    }

    fn set_scan_chain(&mut self, scan_chain: Vec<ScanChainElement>) -> Result<(), DebugProbeError> {
        self.scan_chain = Some(scan_chain);
        Ok(())
    }

    fn scan_chain(&self) -> Result<&[ScanChainElement], DebugProbeError> {
        match &self.scan_chain {
            Some(chain) => Ok(chain),
            None => Err(DebugProbeError::Other(
                "No scan chain set for fake probe".to_string(),
            )),
        }
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
        Err(DebugProbeError::CommandNotSupportedByProbe {
            command_name: "target_reset",
        })
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        unimplemented!()
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
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

    fn jtag_sequence(&mut self, _cycles: u8, _tms: bool, _tdi: u64) -> Result<(), DebugProbeError> {
        todo!()
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

    fn core_status_notification(&mut self, _: crate::CoreStatus) -> Result<(), DebugProbeError> {
        Ok(())
    }
}

#[derive(Debug)]
struct FakeArmInterface<S: ArmDebugState> {
    probe: Box<FakeProbe>,

    state: S,
}

impl FakeArmInterface<Uninitialized> {
    pub(crate) fn new(probe: Box<FakeProbe>) -> Self {
        let state = Uninitialized {
            use_overrun_detect: false,
        };

        Self { probe, state }
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
        dp: DpAddress,
    ) -> Result<Box<dyn ArmProbeInterface>, (Box<dyn UninitializedArmProbe>, Error)> {
        // TODO: Do we need this?
        // sequence.debug_port_setup(&mut self.probe)?;

        let interface = FakeArmInterface::<Initialized> {
            probe: self.probe,
            state: Initialized::new(sequence, dp, false),
        };

        Ok(Box::new(interface))
    }

    fn close(self: Box<Self>) -> Probe {
        Probe::from_attached_probe(self.probe)
    }
}

impl ArmProbeInterface for FakeArmInterface<Initialized> {
    fn memory_interface(
        &mut self,
        access_port: &MemoryAp,
    ) -> Result<Box<dyn ArmMemoryInterface + '_>, ArmError> {
        let ap_information = MemoryApInformation {
            address: access_port.ap_address().clone(),
            supports_only_32bit_data_size: false,
            debug_base_address: 0xf000_0000,
            supports_hnonsec: false,
            has_large_data_extension: false,
            has_large_address_extension: false,
            device_enabled: true,
        };

        match self.probe.memory_ap {
            MockedAp::MemoryAp(ref mut memory_ap) => {
                let memory = ADIMemoryInterface::new(memory_ap, ap_information)
                    .map_err(|e| ArmError::from_access_port(e, access_port))?;

                Ok(Box::new(memory) as _)
            }
            MockedAp::Core(ref mut core) => Ok(Box::new(core) as _),
        }
    }

    fn ap_information(
        &mut self,
        _access_port: &crate::architecture::arm::ap::GenericAp,
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

    fn current_debug_port(&self) -> DpAddress {
        self.state.current_dp
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

    fn read_raw_ap_register(
        &mut self,
        _ap: &FullyQualifiedApAddress,
        _address: u8,
    ) -> Result<u32, ArmError> {
        self.probe.read_raw_ap_register(_ap, _address)
    }

    fn read_raw_ap_register_repeated(
        &mut self,
        _ap: &FullyQualifiedApAddress,
        _address: u8,
        _values: &mut [u32],
    ) -> Result<(), ArmError> {
        todo!()
    }

    fn write_raw_ap_register(
        &mut self,
        _ap: &FullyQualifiedApAddress,
        _address: u8,
        _value: u32,
    ) -> Result<(), ArmError> {
        todo!()
    }

    fn write_raw_ap_register_repeated(
        &mut self,
        _ap: &FullyQualifiedApAddress,
        _address: u8,
        _values: &[u32],
    ) -> Result<(), ArmError> {
        todo!()
    }
}

#[cfg(all(test, feature = "builtin-targets"))]
mod test {
    use super::FakeProbe;
    use crate::Permissions;

    #[test]
    fn create_session_with_fake_probe() {
        let fake_probe = FakeProbe::with_mocked_core();

        let probe = fake_probe.into_probe();

        probe
            .attach("nrf51822_xxAC", Permissions::default())
            .unwrap();
    }
}
