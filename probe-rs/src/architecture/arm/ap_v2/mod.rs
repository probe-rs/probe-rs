use std::collections::BTreeSet;

use crate::{
    architecture::arm::memory::{
        romtable::{RomTable, CORESIGHT_ROM_TABLE_ARCHID},
        Component, PeripheralType,
    },
    MemoryInterface,
};

use super::{
    communication_interface::{Initialized, SwdSequence},
    dp::DpAddress,
    memory::ArmMemoryInterface,
    ApAddress, ApV2Address, ArmCommunicationInterface, ArmError, FullyQualifiedApAddress,
};

mod registers;
mod traits;

mod root_memory_interface;
use root_memory_interface::RootMemoryInterface;

mod memory_access_port_interface;
use memory_access_port_interface::MemoryAccessPortInterface;

enum MemoryAccessPortInterfaces<'iface> {
    Root(RootMemoryInterface<'iface>),
    Node(Box<MemoryAccessPortInterface<'iface>>),
}
impl<'iface> From<RootMemoryInterface<'iface>> for MemoryAccessPortInterfaces<'iface> {
    fn from(value: RootMemoryInterface<'iface>) -> Self {
        Self::Root(value)
    }
}
impl<'iface> From<MemoryAccessPortInterface<'iface>> for MemoryAccessPortInterfaces<'iface> {
    fn from(value: MemoryAccessPortInterface<'iface>) -> Self {
        Self::Node(Box::new(value))
    }
}
macro_rules! dispatch {
    ($name:ident(&mut self, $($arg:ident : $t:ty),*) -> $r:ty) => {
        fn $name(&mut self, $($arg: $t),*) -> $r {
            match self {
                MemoryAccessPortInterfaces::Root(r) => r.$name($($arg),*),
                MemoryAccessPortInterfaces::Node(m) => m.$name($($arg),*),
            }
        }
    };
    ($name:ident(&self, $($arg:ident : $t:ty),*) -> $r:ty) => {
        fn $name(&self, $($arg: $t),*) -> $r {
            match self {
                MemoryAccessPortInterfaces::Root(r) => r.$name($($arg),*),
                MemoryAccessPortInterfaces::Node(m) => m.$name($($arg),*),
            }
        }
    }
}

impl<'iface> SwdSequence for MemoryAccessPortInterfaces<'iface> {
    dispatch!(swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), crate::probe::DebugProbeError>);
    dispatch!(swj_pins(&mut self, pin_out: u32, pin_select: u32, pin_wait: u32) -> Result<u32, crate::probe::DebugProbeError>);
}
impl<'iface> MemoryInterface<ArmError> for MemoryAccessPortInterfaces<'iface> {
    dispatch!(supports_native_64bit_access(&mut self,) -> bool);
    dispatch!(read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), ArmError>);
    dispatch!(read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError>);
    dispatch!(read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), ArmError>);
    dispatch!(read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), ArmError>);
    dispatch!(write_64(&mut self, address: u64, data: &[u64]) -> Result<(), ArmError>);
    dispatch!(write_32(&mut self, address: u64, data: &[u32]) -> Result<(), ArmError>);
    dispatch!(write_16(&mut self, address: u64, data: &[u16]) -> Result<(), ArmError>);
    dispatch!(write_8(&mut self, address: u64, data: &[u8]) -> Result<(), ArmError>);
    dispatch!(supports_8bit_transfers(&self,) -> Result<bool, ArmError>);
    dispatch!(flush(&mut self,) -> Result<(), ArmError>);
}
impl<'iface> ArmMemoryInterface for MemoryAccessPortInterfaces<'iface> {
    dispatch!(ap(&mut self,) -> &mut super::ap_v1::memory_ap::MemoryAp);
    dispatch!(fully_qualified_address(&self,) -> FullyQualifiedApAddress);
    dispatch!(rom_table_address(&mut self,) -> Result<u64, ArmError>);
    dispatch!(get_arm_communication_interface(&mut self,) -> Result<&mut ArmCommunicationInterface<Initialized>, crate::probe::DebugProbeError>);
    dispatch!(try_as_parts(&mut self,) -> Result<(&mut ArmCommunicationInterface<Initialized>, &mut super::ap_v1::memory_ap::MemoryAp), crate::probe::DebugProbeError>);
}

/// Deeply scans the debug port and returns a list of the addresses the memory access points discovered.
pub fn enumerate_access_ports<'i>(
    probe: &'i mut ArmCommunicationInterface<Initialized>,
    dp: DpAddress,
) -> Result<BTreeSet<ApV2Address>, ArmError> {
    let mut root_ap = RootMemoryInterface::new(probe, dp)?;

    let mut result = BTreeSet::new();

    let component = Component::try_parse(&mut root_ap as &mut dyn ArmMemoryInterface, 0)?;
    match component {
        Component::CoresightComponent(c) => {
            if c.peripheral_id().arch_id() == CORESIGHT_ROM_TABLE_ARCHID {
                scan_rom_tables_internal(root_ap, &mut result)?;
            } else if c.peripheral_id().is_of_type(PeripheralType::MemAp) {
                result.insert(ApV2Address::Node(0, Box::new(ApV2Address::Root)));
                let subiface = MemoryAccessPortInterface::new(root_ap, 0)?;
                scan_rom_tables_internal(subiface, &mut result)?;
            }
        }
        _ => {
            // not a coresight component
            return Ok(BTreeSet::new());
        }
    }

    tracing::info!("Memory APs: {:x?}", result);
    Ok(result)
}

fn scan_rom_tables_internal<
    'iface,
    M: Into<MemoryAccessPortInterfaces<'iface>> + ArmMemoryInterface,
>(
    iface: M,
    mem_aps: &mut BTreeSet<ApV2Address>,
) -> Result<MemoryAccessPortInterfaces<'iface>, ArmError> {
    let mut iface = iface.into();
    let rom_table_address = iface.rom_table_address()?;

    let rom_table =
        RomTable::try_parse(&mut iface as &mut dyn ArmMemoryInterface, rom_table_address)?;
    for e in rom_table.entries() {
        if e.component()
            .id()
            .peripheral_id()
            .is_of_type(PeripheralType::MemAp)
        {
            let base = e.component().id().component_address();
            tracing::info!("Found a MemAp at {:x?}", base);

            mem_aps.insert(ApV2Address::Node(base, Box::new(ApV2Address::Root)));

            let subiface = MemoryAccessPortInterface::new(iface, base)?;
            let MemoryAccessPortInterfaces::Node(subiface) =
                scan_rom_tables_internal(subiface, mem_aps)?
            else {
                unreachable!("scan_rom_tables_internal should return the same iface it was given.");
            };
            iface = subiface.release();
        }
    }
    Ok(iface)
}

/// Returns a Memory Interface accessing the Memory AP at the given `address` through the `iface`
/// Arm Communication Interface.
pub fn new_memory_interface<'i>(
    iface: &'i mut ArmCommunicationInterface<Initialized>,
    address: &FullyQualifiedApAddress,
) -> Result<Box<dyn ArmMemoryInterface + 'i>, ArmError> {
    let ApAddress::V2(ap_address) = address.ap() else {
        unimplemented!("this is only for APv2 addresses")
    };

    new_memory_interface_inner(iface, address.dp(), ap_address)
        .map(|iface| Box::new(iface) as Box<dyn ArmMemoryInterface + 'i>)
}

fn new_memory_interface_inner<'i>(
    iface: &'i mut ArmCommunicationInterface<Initialized>,
    dp: DpAddress,
    address: &ApV2Address,
) -> Result<MemoryAccessPortInterface<'i>, ArmError> {
    tracing::trace!("address: {:x?}", address);
    match address {
        ApV2Address::Node(base, ap) if matches!(**ap, ApV2Address::Root) => {
            let root = RootMemoryInterface::new(iface, dp)?;
            MemoryAccessPortInterface::new(root, *base)
        }
        ApV2Address::Node(base, ap) => {
            let subiface = new_memory_interface_inner(iface, dp, ap)?;
            MemoryAccessPortInterface::new(subiface, *base)
        }
        _ => unreachable!(),
    }
}
