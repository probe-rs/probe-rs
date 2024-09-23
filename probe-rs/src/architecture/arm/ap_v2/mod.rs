use std::collections::BTreeSet;

use crate::MemoryInterface;

use super::{
    communication_interface::{Initialized, SwdSequence}, memory::ArmMemoryInterface, ApAddress, ApV2Address,
    ArmCommunicationInterface, ArmError, FullyQualifiedApAddress,
};

type ACI = ArmCommunicationInterface<Initialized>;

struct MemoryAccessPort<'i> {
    iface: Box<dyn ArmMemoryInterface + 'i>,
    base: u64,
}
impl<'i> MemoryAccessPort<'i> {
    fn new(iface: Box<dyn ArmMemoryInterface + 'i>, base: u64) -> Result<Self, ArmError> {
        // TODO! validity check from the parent root table
        Ok(Self { iface, base })
    }
}
impl SwdSequence for MemoryAccessPort<'_> {
    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), crate::probe::DebugProbeError> {
        todo!()
    }

    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, crate::probe::DebugProbeError> {
        todo!()
    }
}
impl MemoryInterface<ArmError> for MemoryAccessPort<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        todo!()
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), ArmError> {
        todo!()
    }

    fn supports_8bit_transfers(&self) -> Result<bool, ArmError> {
        todo!()
    }

    fn flush(&mut self) -> Result<(), ArmError> {
        todo!()
    }
}
impl ArmMemoryInterface for MemoryAccessPort<'_> {
    fn ap(&mut self) -> &mut super::ap_v1::memory_ap::MemoryAp {
        todo!()
    }

    fn base_address(&mut self) -> Result<u64, ArmError> {
        todo!()
    }

    fn get_arm_communication_interface(
        &mut self,
    ) -> Result<&mut ArmCommunicationInterface<Initialized>, crate::probe::DebugProbeError> {
        todo!()
    }

    fn try_as_parts(
        &mut self,
    ) -> Result<(&mut ArmCommunicationInterface<Initialized>, &mut super::ap_v1::memory_ap::MemoryAp), crate::probe::DebugProbeError> {
        todo!()
    }
}

struct RootMemoryAp<'iface> {
    iface: &'iface mut ACI,
    base: u64,
}
impl<'iface> RootMemoryAp<'iface> {
    fn new(iface: &'iface mut ACI, base: u64) -> Result<Self, ArmError> {
        // TODO! validity check from the DP’s root table
        Ok(Self { iface, base })
    }
}
impl SwdSequence for RootMemoryAp<'_> {
    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), crate::probe::DebugProbeError> {
        todo!()
    }

    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, crate::probe::DebugProbeError> {
        todo!()
    }
}
impl MemoryInterface<ArmError> for RootMemoryAp<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        todo!()
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), ArmError> {
        todo!()
    }

    fn supports_8bit_transfers(&self) -> Result<bool, ArmError> {
        todo!()
    }

    fn flush(&mut self) -> Result<(), ArmError> {
        todo!()
    }
}
impl ArmMemoryInterface for RootMemoryAp<'_> {
    fn ap(&mut self) -> &mut super::ap_v1::memory_ap::MemoryAp {
        todo!()
    }

    fn base_address(&mut self) -> Result<u64, ArmError> {
        todo!()
    }

    fn get_arm_communication_interface(
        &mut self,
    ) -> Result<&mut ArmCommunicationInterface<Initialized>, crate::probe::DebugProbeError> {
        todo!()
    }

    fn try_as_parts(
        &mut self,
    ) -> Result<(&mut ArmCommunicationInterface<Initialized>, &mut super::ap_v1::memory_ap::MemoryAp), crate::probe::DebugProbeError> {
        todo!()
    }
}

pub fn enumerate_access_ports(
    _probe: &mut ACI,
) -> Result<BTreeSet<ApV2Address>, ArmError> {
    // get root base address
    //
    // build a root memory interface
    //let rom_table = RomTable::try_parse(memory, base_address)?;
    //for e in rom_table.entries() {
    // if e is a mem_ap
    //  add it to the set
    //  process its rom_table
    //}
    todo!()
}

fn scan_rom_tables_internal(
    _probe: &mut ACI,
    _mem_aps: &mut BTreeSet<ApAddress>,
    _current_addr: Option<ApV2Address>,
) -> Result<(), ArmError> {
    todo!()
}

pub fn new_memory_interface<'i>(
    iface: &'i mut ACI,
    address: &FullyQualifiedApAddress,
) -> Result<Box<dyn ArmMemoryInterface + 'i>, ArmError> {
    let ApAddress::V2(address) = address.ap() else {
        unimplemented!("this is only for APv2 addresses")
    };

    let mut next = None;
    let base = match address {
        ApV2Address::Leaf(base) => base,
        ApV2Address::Node(base, n) => {
            next = Some(n);
            base
        }
    };
    let mut ap: Box<dyn ArmMemoryInterface + 'i> = Box::new(RootMemoryAp::new(iface, *base)?);
    while let Some(n) = next {
        match n.as_ref() {
            ApV2Address::Leaf(base) => {
                ap = Box::new(MemoryAccessPort::new(ap, *base)?);
                next = None;
            }
            ApV2Address::Node(base, n) => {
                ap = Box::new(MemoryAccessPort::new(ap, *base)?);
                next = Some(n);
            }
        }
    }

    Ok(ap)
}
