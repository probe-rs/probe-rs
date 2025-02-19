#![allow(missing_docs)] // Don't require docs for test code
use crate::{
    Error, MemoryInterface, MemoryMappedRegister,
    architecture::arm::{
        ArmError, ArmProbeInterface, DapAccess, FullyQualifiedApAddress, RawDapAccess,
        RegisterAddress, SwoAccess,
        ap::memory_ap::mock::MockMemoryAp,
        armv8m::Dhcsr,
        communication_interface::{
            ArmDebugState, DapProbe, Initialized, SwdSequence, Uninitialized, UninitializedArmProbe,
        },
        dp::{DpAddress, DpRegisterAddress},
        memory::{ADIMemoryInterface, ArmMemoryInterface},
        sequences::ArmDebugSequence,
    },
    probe::{DebugProbe, DebugProbeError, Probe, WireProtocol},
};
use object::{
    Endianness, Object, ObjectSection,
    elf::{FileHeader32, FileHeader64, PT_LOAD},
    read::elf::{ElfFile, FileHeader, ProgramHeader},
};
use probe_rs_target::{MemoryRange, ScanChainElement};
use std::{
    cell::RefCell,
    collections::{BTreeSet, VecDeque},
    fmt::Debug,
    path::Path,
    sync::Arc,
};

/// This is a mock probe which can be used for mocking things in tests or for dry runs.
#[allow(clippy::type_complexity)]
pub struct FakeProbe {
    protocol: WireProtocol,
    speed: u32,
    scan_chain: Option<Vec<ScanChainElement>>,

    dap_register_read_handler: Option<Box<dyn Fn(RegisterAddress) -> Result<u32, ArmError> + Send>>,

    dap_register_write_handler:
        Option<Box<dyn Fn(RegisterAddress, u32) -> Result<(), ArmError> + Send>>,

    operations: RefCell<VecDeque<Operation>>,

    memory_ap: MockedAp,
}

enum MockedAp {
    /// Mock a memory AP
    MemoryAp(MockMemoryAp),
    /// Mock an ARM core behind a memory AP
    Core(MockCore),
}

struct LoadableSegment {
    physical_address: u64,
    offset: u64,
    size: u64,
}

impl LoadableSegment {
    fn contains(&self, physical_address: u64, len: u64) -> bool {
        physical_address >= self.physical_address
            && physical_address < (self.physical_address + self.size - len)
    }

    fn load_addr(&self, physical_address: u64) -> u64 {
        let offset_in_segment = physical_address - self.physical_address;
        self.offset + offset_in_segment
    }
}

struct MockCore {
    dhcsr: Dhcsr,

    /// Is the core halted?
    is_halted: bool,

    program_binary: Option<Vec<u8>>,
    loadable_segments: Vec<LoadableSegment>,
    endianness: Endianness,
}

impl MockCore {
    pub fn new() -> Self {
        Self {
            dhcsr: Dhcsr(0),
            is_halted: false,
            program_binary: None,
            loadable_segments: Vec::new(),
            endianness: Endianness::Little,
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
        let mut curr_seg: Option<&LoadableSegment> = None;

        for (offset, val) in data.iter_mut().enumerate() {
            let address = address + offset as u64;
            println!("Read {:#010x} = 0", address);

            match self.program_binary {
                Some(ref program_binary) => {
                    if !curr_seg.is_some_and(|seg| seg.contains(address, 1)) {
                        curr_seg = self
                            .loadable_segments
                            .iter()
                            .find(|&seg| seg.contains(address, 1));
                    }
                    match curr_seg {
                        Some(seg) => {
                            *val = program_binary[seg.load_addr(address) as usize];
                        }
                        None => *val = 0,
                    }
                }
                None => *val = 0,
            }
        }

        Ok(())
    }

    fn read_16(&mut self, _address: u64, _data: &mut [u16]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError> {
        let mut curr_seg: Option<&LoadableSegment> = None;

        for (offset, val) in data.iter_mut().enumerate() {
            const U32_BYTES: usize = 4;
            let address = address + (offset * U32_BYTES) as u64;

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

                address => {
                    println!("Read {:#010x} = 0", address);

                    match self.program_binary {
                        Some(ref program_binary) => {
                            if !curr_seg.is_some_and(|seg| seg.contains(address, U32_BYTES as u64))
                            {
                                curr_seg = self
                                    .loadable_segments
                                    .iter()
                                    .find(|&seg| seg.contains(address, U32_BYTES as u64));
                            }
                            match curr_seg {
                                Some(seg) => {
                                    let from = seg.load_addr(address) as usize;
                                    let to = from + U32_BYTES;

                                    let u32_as_bytes: [u8; U32_BYTES] =
                                        program_binary[from..to].try_into().unwrap();

                                    // Convert to _host_ (native) endianness.
                                    *val = if self.endianness == Endianness::Little {
                                        u32::from_le_bytes(u32_as_bytes)
                                    } else {
                                        u32::from_be_bytes(u32_as_bytes)
                                    };
                                }
                                None => *val = 0,
                            }
                        }
                        None => *val = 0,
                    }
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
}

impl ArmMemoryInterface for &mut MockCore {
    fn base_address(&mut self) -> Result<u64, ArmError> {
        todo!()
    }

    fn fully_qualified_address(&self) -> FullyQualifiedApAddress {
        todo!()
    }

    fn get_arm_probe_interface(&mut self) -> Result<&mut dyn ArmProbeInterface, DebugProbeError> {
        todo!()
    }

    fn get_swd_sequence(&mut self) -> Result<&mut dyn SwdSequence, DebugProbeError> {
        todo!()
    }

    fn get_dap_access(&mut self) -> Result<&mut dyn DapAccess, DebugProbeError> {
        todo!()
    }

    fn generic_status(&mut self) -> Result<crate::architecture::arm::ap::CSW, ArmError> {
        todo!()
    }

    fn update_core_status(&mut self, _state: crate::CoreStatus) {}
}

#[derive(Debug, Clone, PartialEq)]
pub enum Operation {
    ReadRawApRegister {
        ap: FullyQualifiedApAddress,
        address: u64,
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
            memory_ap: MockedAp::Core(MockCore::new()),
            ..Self::default()
        }
    }

    /// Fake probe with a mocked core
    /// with access to an actual binary file.
    pub fn with_mocked_core_and_binary(program_binary: &Path) -> Self {
        let file_data = std::fs::read(program_binary).unwrap();
        let file_data_slice = file_data.as_slice();

        let file_kind = object::FileKind::parse(file_data.as_slice()).unwrap();
        let core = match file_kind {
            object::FileKind::Elf32 => core_with_binary(
                object::read::elf::ElfFile::<FileHeader32<Endianness>>::parse(file_data_slice)
                    .unwrap(),
            ),
            object::FileKind::Elf64 => core_with_binary(
                object::read::elf::ElfFile::<FileHeader64<Endianness>>::parse(file_data_slice)
                    .unwrap(),
            ),
            _ => {
                unimplemented!("unsupported file format")
            }
        };

        FakeProbe {
            memory_ap: MockedAp::Core(core),
            ..Self::default()
        }
    }

    /// This sets the read handler for DAP register reads.
    /// Can be used to hook into the read.
    pub fn set_dap_register_read_handler(
        &mut self,
        handler: Box<dyn Fn(RegisterAddress) -> Result<u32, ArmError> + Send>,
    ) {
        self.dap_register_read_handler = Some(handler);
    }

    /// This sets the write handler for DAP register writes.
    /// Can be used to hook into the write.
    pub fn set_dap_register_write_handler(
        &mut self,
        handler: Box<dyn Fn(RegisterAddress, u32) -> Result<(), ArmError> + Send>,
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
        expected_address: u64,
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
            None => panic!(
                "No more operations expected, but got read_raw_ap_register ap={expected_ap:?}, address:{expected_address}"
            ),
            //other => panic!("Unexpected operation: {:?}", other),
        }
    }

    pub fn expect_operation(&self, operation: Operation) {
        self.operations.borrow_mut().push_back(operation);
    }
}

fn core_with_binary<T: FileHeader>(elf_file: ElfFile<T>) -> MockCore {
    let elf_header = elf_file.elf_header();
    let elf_data = elf_file.data();
    let endian = elf_header.endian().unwrap();

    let mut loadable_sections = Vec::new();
    for segment in elf_header.program_headers(endian, elf_data).unwrap() {
        let physical_address = segment.p_paddr(endian).into();
        let segment_data = segment.data(endian, elf_data).unwrap();

        if !segment_data.is_empty() && segment.p_type(endian) == PT_LOAD {
            let (segment_offset, segment_filesize) = segment.file_range(endian);
            let segment_range = segment_offset..segment_offset + segment_filesize;

            let mut found_section_in_segment = false;

            for section in elf_file.sections() {
                let (section_offset, section_filesize) = match section.file_range() {
                    Some(range) => range,
                    None => continue,
                };

                if segment_range
                    .contains_range(&(section_offset..section_offset + section_filesize))
                {
                    found_section_in_segment = true;
                    break;
                }
            }

            if found_section_in_segment {
                loadable_sections.push(LoadableSegment {
                    physical_address,
                    offset: segment_offset,
                    size: segment_filesize,
                });
            }
        }
    }

    let mut core = MockCore::new();
    core.program_binary = Some(elf_data.to_owned());
    core.loadable_segments = loadable_sections;
    core.endianness = if elf_header.is_little_endian() {
        Endianness::Little
    } else {
        Endianness::Big
    };

    core
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
    fn raw_read_register(&mut self, address: RegisterAddress) -> Result<u32, ArmError> {
        let handler = self.dap_register_read_handler.as_ref().unwrap();

        handler(address)
    }

    /// Writes a value to the DAP register on the specified port and address
    fn raw_write_register(&mut self, address: RegisterAddress, value: u32) -> Result<(), ArmError> {
        let handler = self.dap_register_write_handler.as_ref().unwrap();

        handler(address, value)
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

    fn reinitialize(&mut self) -> Result<(), ArmError> {
        Ok(())
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
    fn read_raw_dp_register(
        &mut self,
        _dp: DpAddress,
        _address: DpRegisterAddress,
    ) -> Result<u32, ArmError> {
        todo!()
    }

    fn write_raw_dp_register(
        &mut self,
        _dp: DpAddress,
        _address: DpRegisterAddress,
        _value: u32,
    ) -> Result<(), ArmError> {
        todo!()
    }

    fn read_raw_ap_register(
        &mut self,
        _ap: &FullyQualifiedApAddress,
        _address: u64,
    ) -> Result<u32, ArmError> {
        self.probe.read_raw_ap_register(_ap, _address)
    }

    fn read_raw_ap_register_repeated(
        &mut self,
        _ap: &FullyQualifiedApAddress,
        _address: u64,
        _values: &mut [u32],
    ) -> Result<(), ArmError> {
        todo!()
    }

    fn write_raw_ap_register(
        &mut self,
        _ap: &FullyQualifiedApAddress,
        _address: u64,
        _value: u32,
    ) -> Result<(), ArmError> {
        todo!()
    }

    fn write_raw_ap_register_repeated(
        &mut self,
        _ap: &FullyQualifiedApAddress,
        _address: u64,
        _values: &[u32],
    ) -> Result<(), ArmError> {
        todo!()
    }

    fn try_dap_probe(&self) -> Option<&dyn DapProbe> {
        None
    }

    fn try_dap_probe_mut(&mut self) -> Option<&mut dyn DapProbe> {
        None
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
