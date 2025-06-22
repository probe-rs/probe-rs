//! APv2 support for ADIv6

use std::collections::BTreeSet;

use crate::architecture::arm::{
    ApAddress, ApV2Address, ArmCommunicationInterface, ArmDebugInterface, ArmError,
    FullyQualifiedApAddress,
    dp::DpAddress,
    memory::{
        ADIMemoryInterface, ArmMemoryInterface, Component, PeripheralType,
        romtable::{CORESIGHT_ROM_TABLE_ARCHID, RomTable},
    },
};

mod root_memory_interface;
use root_memory_interface::RootMemoryInterface;

/// Deeply scans the debug port and returns a list of the addresses the memory access points discovered.
pub fn enumerate_access_ports<ADI: ArmDebugInterface>(
    probe: &mut ADI,
    dp: DpAddress,
) -> Result<BTreeSet<FullyQualifiedApAddress>, ArmError> {
    let mut root_interface = RootMemoryInterface::new(probe, dp)?;
    let base_addr = root_interface.base_address()?;

    let root_component = Component::try_parse(
        &mut root_interface as &mut dyn ArmMemoryInterface,
        base_addr,
    )?;

    let result = process_root_component(&mut root_interface, &root_component)?;

    Ok(result
        .into_iter()
        .map(|addr| FullyQualifiedApAddress::v2_with_dp(dp, addr))
        .collect())
}

fn process_root_component<ADI: ArmDebugInterface>(
    iface: &mut RootMemoryInterface<ADI>,
    component: &Component,
) -> Result<BTreeSet<ApV2Address>, ArmError> {
    let mut result = BTreeSet::new();

    match component {
        Component::CoresightComponent(c)
            if c.peripheral_id().arch_id() == CORESIGHT_ROM_TABLE_ARCHID =>
        {
            let rom_table = RomTable::try_parse(iface, c.component_address())?;
            for e in rom_table.entries() {
                if let Component::CoresightComponent(comp) = e.component() {
                    if comp.peripheral_id().is_of_type(PeripheralType::MemAp) {
                        let base_address = ApV2Address::new(comp.component_address());
                        // TODO: Check this AP for further nested APs.
                        result.insert(base_address);
                    }
                }
            }
        }
        Component::Class1RomTable(_, rom_table) => {
            for e in rom_table.entries() {
                if let Component::CoresightComponent(comp) = e.component() {
                    if comp.peripheral_id().is_of_type(PeripheralType::MemAp) {
                        let base_address = ApV2Address::new(comp.component_address());
                        // TODO: Check this AP for further nested APs.
                        result.insert(base_address);
                    }
                }
            }
        }

        // If the root component is a memory AP, it's the only component in the system and we can
        // return it immediately.
        Component::CoresightComponent(c) if c.peripheral_id().is_of_type(PeripheralType::MemAp) => {
            let base_address = ApV2Address::new(c.component_address());
            // TODO: Check this AP for further nested APs.
            result.insert(base_address);
        }

        _ => {}
    }

    Ok(result)
}

/// Returns a Memory Interface accessing the Memory AP at the given `address` through the `iface`
/// Arm Communication Interface.
pub fn new_memory_interface<'i>(
    iface: &'i mut ArmCommunicationInterface,
    address: &FullyQualifiedApAddress,
) -> Result<Box<dyn ArmMemoryInterface + 'i>, ArmError> {
    let ApAddress::V2(ap_address) = address.ap() else {
        unimplemented!("this is only for APv2 addresses")
    };

    if ap_address.0.is_none() {
        Ok(Box::new(RootMemoryInterface::new(iface, address.dp())?))
    } else {
        Ok(Box::new(ADIMemoryInterface::new(iface, address)?))
    }
}
