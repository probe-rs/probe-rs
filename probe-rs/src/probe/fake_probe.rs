#![allow(missing_docs)] // Don't require docs for test code
use std::{
    cell::RefCell,
    collections::{BTreeSet, VecDeque},
    fmt::Debug,
    sync::Arc,
};

use probe_rs_target::ScanChainElement;

use crate::{
    architecture::arm::{
        ap::memory_ap::{mock::MockMemoryAp, MemoryAp},
        armv8m::Dhcsr,
        communication_interface::{
            ArmDebugState, Initialized, SwdSequence, Uninitialized, UninitializedArmProbe,
        },
        memory::{adi_v5_memory_interface::ADIMemoryInterface, ArmMemoryInterface},
        sequences::ArmDebugSequence,
        ArmError, ArmProbeInterface, DapAccess, DpAddress, FullyQualifiedApAddress, PortType,
        RawDapAccess, SwoAccess,
    },
    probe::{DebugProbe, DebugProbeError, Probe, WireProtocol},
    Error, MemoryInterface, MemoryMappedRegister,
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

pub type ArmReadFn = Box<dyn Fn(u64, &mut [u8]) -> Result<(), ArmError> + Send>;
pub type ArmWriteFn = Box<dyn Fn(u64, &[u8]) -> Result<(), ArmError> + Send>;
struct MockCore {
    dhcsr: Dhcsr,

    arm_read_handler: Option<ArmReadFn>,
    arm_write_handler: Option<ArmWriteFn>,

    /// Is the core halted?
    is_halted: bool,
}

impl MockCore {
    pub fn new() -> Self {
        Self {
            dhcsr: Dhcsr(0),
            arm_read_handler: None,
            arm_write_handler: None,
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

impl MemoryInterface<ArmError> for &mut MockCore {
    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), ArmError> {
        self.arm_read_handler.as_ref().unwrap()(address, data)
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), ArmError> {
        self.arm_read_handler.as_ref().unwrap()(address, unsafe {
            std::slice::from_raw_parts_mut(data.as_mut_ptr() as *mut u8, data.len() * 2)
        })
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError> {
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

                data[0] = dhcsr;
                tracing::trace!("[read_32] <DHCSR>: {:#x} = {:#x}", address, data[0]);
                Ok(())
            }
            0xE0000000..=0xE00FFFFF => {
                tracing::trace!("[read_32] <debug protocol>: {:08x} = {}", address, data[0]);
                Ok(())
            }
            _ => self.arm_read_handler.as_ref().unwrap()(address, unsafe {
                std::slice::from_raw_parts_mut(data.as_mut_ptr() as *mut u8, data.len() * 4)
            }),
        }
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), ArmError> {
        self.arm_read_handler.as_ref().unwrap()(address, unsafe {
            std::slice::from_raw_parts_mut(data.as_mut_ptr() as *mut u8, data.len() * 8)
        })
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), ArmError> {
        self.arm_write_handler.as_ref().unwrap()(address, data)
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), ArmError> {
        self.arm_write_handler.as_ref().unwrap()(address, unsafe {
            std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2)
        })
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), ArmError> {
        match address {
            Dhcsr::ADDRESS_OFFSET => {
                let dbg_key = (data[0] >> 16) & 0xffff;

                if dbg_key == 0xa05f {
                    // Mask out dbg key
                    self.dhcsr = Dhcsr::from(data[0] & 0xffff);
                    tracing::trace!("[write_32] <DHCSR> = {:#010x}", data[0]);

                    let request_halt = self.dhcsr.c_halt();

                    self.is_halted = request_halt;

                    if !self.dhcsr.c_halt() && self.dhcsr.c_debugen() && self.dhcsr.c_step() {
                        tracing::debug!("MockCore: Single step requested, setting s_halt");
                        self.is_halted = true;
                    }
                }

                Ok(())
            }
            0xE0000000..=0xE00FFFFF => {
                tracing::trace!("[write_32] <debug protocol>: {:08x} = {}", address, data[0]);
                Ok(())
            }
            _ => self.arm_write_handler.as_ref().unwrap()(address, unsafe {
                std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4)
            }),
        }
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), ArmError> {
        self.arm_write_handler.as_ref().unwrap()(address, unsafe {
            std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 8)
        })
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
}

impl ArmMemoryInterface for &mut MockCore {
    fn base_address(&mut self) -> Result<u64, ArmError> {
        todo!()
    }

    fn ap(&mut self) -> &mut MemoryAp {
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

    fn try_as_parts(
        &mut self,
    ) -> Result<
        (
            &mut crate::architecture::arm::ArmCommunicationInterface<Initialized>,
            &mut MemoryAp,
        ),
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

    pub fn set_arm_read_handler(&mut self, handler: ArmReadFn) -> Result<(), anyhow::Error> {
        if let MockedAp::Core(core) = &mut self.memory_ap {
            core.arm_read_handler = Some(handler);
            Ok(())
        } else {
            Err(anyhow::anyhow!("No core to set read handler on MemoryAP"))
        }
    }

    pub fn set_arm_write_handler(&mut self, handler: ArmWriteFn) -> Result<(), anyhow::Error> {
        if let MockedAp::Core(core) = &mut self.memory_ap {
            core.arm_write_handler = Some(handler);
            Ok(())
        } else {
            Err(anyhow::anyhow!("No core to set read handler on MemoryAP"))
        }
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

                tracing::trace!("[read_raw_ap_register] Read from {address:#x}, returned {result}");

                Ok(result)
            }
            None => panic!("[read_raw_ap_register] No more operations expected, but got read_raw_ap_register ap={expected_ap:?}, address:{expected_address}"),
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

impl crate::architecture::arm::communication_interface::FlushableArmAccess
    for FakeArmInterface<Initialized>
{
    fn flush(&mut self) -> Result<(), ArmError> {
        todo!()
    }
}

impl ArmProbeInterface for FakeArmInterface<Initialized> {
    fn memory_interface(
        &mut self,
        access_port_address: &FullyQualifiedApAddress,
    ) -> Result<Box<dyn ArmMemoryInterface + '_>, ArmError> {
        match self.probe.memory_ap {
            MockedAp::MemoryAp(ref mut _memory_ap) => {
                let memory = ADIMemoryInterface::new(self, access_port_address)?;

                Ok(Box::new(memory) as _)
            }
            MockedAp::Core(ref mut core) => Ok(Box::new(core) as _),
        }
    }

    fn access_ports(
        &mut self,
        dp: DpAddress,
    ) -> Result<BTreeSet<FullyQualifiedApAddress>, ArmError> {
        Ok(BTreeSet::from([FullyQualifiedApAddress::v1_with_dp(dp, 1)]))
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
    use std::sync::{Arc, Mutex};

    use super::{FakeProbe, Operation};
    use crate::{architecture::arm::FullyQualifiedApAddress, MemoryInterface, Permissions};

    #[test]
    fn create_session_with_fake_probe() {
        let fake_probe = FakeProbe::with_mocked_core();

        let probe = fake_probe.into_probe();

        probe
            .attach("nrf51822_xxAC", Permissions::default())
            .unwrap();
    }

    #[test]
    fn arm_read_write() {
        let fake_register = Arc::new(Mutex::new([0u8; 4]));

        let mut fake_probe = FakeProbe::with_mocked_core();
        fake_probe
            .set_arm_read_handler({
                let fake_register = fake_register.clone();
                Box::new(move |addr, data| {
                    println!(">>>> Read from {:#x} for {} bytes", addr, data.len());
                    assert_eq!(data.len(), 4);

                    let reference = fake_register.lock().unwrap();
                    data.copy_from_slice(reference.as_ref());

                    Ok(())
                })
            })
            .unwrap();
        fake_probe
            .set_arm_write_handler({
                let fake_register = fake_register.clone();
                Box::new(move |addr, data| {
                    println!(">>>> Write to {:#x} for {} bytes", addr, data.len());

                    fake_register.lock().unwrap().copy_from_slice(data);

                    Ok(())
                })
            })
            .unwrap();

        // https://docs.nordicsemi.com/bundle/ps_nrf52833/page/dif.html#register.APPROTECTSTATUS
        fake_probe.expect_operation(Operation::ReadRawApRegister {
            ap: FullyQualifiedApAddress::v1_with_default_dp(1),
            address: 0x0c,
            result: 0x1,
        });

        let probe = fake_probe.into_probe();
        let mut session = probe
            .attach("nRF52833_xxAA", Permissions::default())
            .unwrap();
        let mut core = session.core(0).unwrap();

        let mut data = [0xFF; 4];
        core.read(0x2000_0000, &mut data).unwrap();
        assert_eq!(data, [0, 0, 0, 0]);

        data = [0x12, 0x34, 0x56, 0x78];
        core.write(0x2000_0000, &data).unwrap();
        assert_eq!(
            fake_register.lock().unwrap().as_ref(),
            [0x12, 0x34, 0x56, 0x78]
        );

        data = [0xFF, 0xFF, 0xFF, 0xFF];
        core.read(0x2000_0000, &mut data).unwrap();
        assert_eq!(data, [0x12, 0x34, 0x56, 0x78]);
    }
}
