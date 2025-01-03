use std::collections::{BTreeMap, BTreeSet};

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

enum MaybeOwned<'i> {
    Reference(&'i mut (dyn ArmMemoryInterface + 'i)),
    Boxed(Box<dyn ArmMemoryInterface + 'i>),
}
macro_rules! dispatch {
    ($name:ident(&mut self, $($arg:ident : $t:ty),*) -> $r:ty) => {
        fn $name(&mut self, $($arg: $t),*) -> $r {
            match self {
                MaybeOwned::Reference(r) => r.$name($($arg),*),
                MaybeOwned::Boxed(b) => b.$name($($arg),*),
            }
        }
    };
    ($name:ident(&self, $($arg:ident : $t:ty),*) -> $r:ty) => {
        fn $name(&self, $($arg: $t),*) -> $r {
            match self {
                MaybeOwned::Reference(r) => r.$name($($arg),*),
                MaybeOwned::Boxed(b) => b.$name($($arg),*),
            }
        }
    }
}

impl SwdSequence for MaybeOwned<'_> {
    dispatch!(swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), crate::probe::DebugProbeError>);
    dispatch!(swj_pins(&mut self, pin_out: u32, pin_select: u32, pin_wait: u32) -> Result<u32, crate::probe::DebugProbeError>);
}
impl MemoryInterface<ArmError> for MaybeOwned<'_> {
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
impl ArmMemoryInterface for MaybeOwned<'_> {
    dispatch!(fully_qualified_address(&self,) -> FullyQualifiedApAddress);
    dispatch!(base_address(&mut self,) -> Result<u64, ArmError>);
    dispatch!(get_arm_communication_interface(&mut self,) -> Result<&mut ArmCommunicationInterface<Initialized>, crate::probe::DebugProbeError>);
    dispatch!(try_as_parts(&mut self,) -> Result<(&mut ArmCommunicationInterface<Initialized>, &mut crate::architecture::arm::ap_v1::memory_ap::MemoryAp), crate::probe::DebugProbeError>);
}

/// Deeply scans the debug port and returns a list of the addresses the memory access points discovered.
pub fn enumerate_access_ports(
    probe: &mut ArmCommunicationInterface<Initialized>,
    dp: DpAddress,
) -> Result<BTreeSet<FullyQualifiedApAddress>, ArmError> {
    enumerate_components_internal(probe, dp).map(|res| {
        res.into_iter()
            .filter_map(|(k, c)| {
                c.id()
                    .peripheral_id()
                    .is_of_type(PeripheralType::MemAp)
                    .then_some(k)
            })
            .map(|addr| FullyQualifiedApAddress::v2_with_dp(dp, addr))
            .collect()
    })
}

/// Enumerates components attached to this debug port
pub fn enumerate_components(
    probe: &mut ArmCommunicationInterface<Initialized>,
    dp: DpAddress,
) -> Result<BTreeSet<FullyQualifiedApAddress>, ArmError> {
    enumerate_components_internal(probe, dp).map(|res| {
        res.into_keys()
            .map(|addr| FullyQualifiedApAddress::v2_with_dp(dp, addr))
            .collect()
    })
}

fn enumerate_components_internal(
    probe: &mut ArmCommunicationInterface<Initialized>,
    dp: DpAddress,
) -> Result<BTreeMap<ApV2Address, Component>, ArmError> {
    let mut root_ap = RootMemoryInterface::new(probe, dp)?;
    let base_addr = root_ap.base_address()?;

    let mut result = BTreeMap::new();

    let component = Component::try_parse(&mut root_ap as &mut dyn ArmMemoryInterface, base_addr)?;
    process_component(&mut root_ap, &ApV2Address::new(), &component, &mut result)?;

    Ok(result)
}

fn process_component<'iface, M: ArmMemoryInterface + 'iface>(
    iface: &'iface mut M,
    address: &ApV2Address,
    component: &Component,
    result: &mut BTreeMap<ApV2Address, Component>,
) -> Result<(), ArmError> {
    match component {
        Component::CoresightComponent(c)
            if c.peripheral_id().arch_id() == CORESIGHT_ROM_TABLE_ARCHID =>
        {
            // read rom table
            let rom_table =
                RomTable::try_parse(iface as &mut dyn ArmMemoryInterface, c.component_address())?;
            // process rom table
            for e in rom_table.entries() {
                process_component(iface, address, e.component(), result)?;
            }
        }
        Component::CoresightComponent(c) if c.peripheral_id().is_of_type(PeripheralType::MemAp) => {
            let base_address = address.clone().append(c.component_address());

            let mut subiface = MemoryAccessPortInterface::new_with_ref(
                iface as &mut dyn ArmMemoryInterface,
                c.component_address(),
            )?;
            let base_addr = subiface.base_address()?;

            let memap_base_component =
                Component::try_parse(&mut subiface as &mut dyn ArmMemoryInterface, base_addr)?;
            process_component(&mut subiface, &base_address, &memap_base_component, result)?;
            result.insert(base_address, component.clone());
        }
        Component::Class1RomTable(_, rom_table) => {
            for e in rom_table.entries() {
                process_component(&mut *iface, address, e.component(), result)?;
            }
        }
        _ => {
            let address = address.clone().append(component.id().component_address());
            result.insert(address, component.clone());
        }
    }
    Ok(())
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

    new_memory_interface_internal(iface, address.dp(), ap_address.as_slice())
}

fn new_memory_interface_internal<'i>(
    iface: &'i mut ArmCommunicationInterface<Initialized>,
    dp: DpAddress,
    address: &[u64],
) -> Result<Box<dyn ArmMemoryInterface + 'i>, ArmError> {
    Ok(match address {
        [ap @ .., base] => {
            let subiface = new_memory_interface_internal(iface, dp, ap)?;
            Box::new(MemoryAccessPortInterface::boxed(subiface, *base)?)
                as Box<dyn ArmMemoryInterface + 'i>
        }
        [] => Box::new(RootMemoryInterface::new(iface, dp)?) as Box<dyn ArmMemoryInterface + 'i>,
    })
}
