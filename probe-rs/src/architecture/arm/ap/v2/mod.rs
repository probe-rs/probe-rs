//! APv2 support for ADIv6

use std::collections::{BTreeMap, BTreeSet};

use crate::architecture::arm::{
    communication_interface::Initialized,
    dp::DpAddress,
    memory::{
        romtable::{RomTable, CORESIGHT_ROM_TABLE_ARCHID},
        ADIMemoryInterface, ArmMemoryInterface, Component, PeripheralType,
    },
    ApAddress, ApV2Address, ArmCommunicationInterface, ArmError, FullyQualifiedApAddress,
};

mod root_memory_interface;
use root_memory_interface::RootMemoryInterface;

mod memory_access_port_interface;
use memory_access_port_interface::MemoryAccessPortInterface;

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
    process_component(&mut root_ap, &ApV2Address::root(), &component, &mut result)?;

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

            let mut subiface = MemoryAccessPortInterface::new(
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

    if ap_address.as_slice().is_empty() {
        Ok(Box::new(RootMemoryInterface::new(iface, address.dp())?)
            as Box<dyn ArmMemoryInterface + 'i>)
    } else {
        Ok(Box::new(ADIMemoryInterface::new(iface, address)?))
    }
}
