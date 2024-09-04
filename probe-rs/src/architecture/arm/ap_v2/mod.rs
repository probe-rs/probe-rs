use std::collections::BTreeSet;

use registers::Register;
use traits::ApAccess;

use crate::{
    architecture::arm::{
        dp::{DpAccess, BASEPTR0, BASEPTR1, DPIDR1},
        memory::romtable::RomTable,
    },
    MemoryInterface,
};

use super::{
    communication_interface::{Initialized, SwdSequence},
    dp::DpAddress,
    memory::ArmMemoryInterface,
    ApAddress, ApV2Address, ArmCommunicationInterface, ArmError, DapAccess,
    FullyQualifiedApAddress,
};

mod registers;
mod traits;

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
    fn swj_sequence(
        &mut self,
        _bit_len: u8,
        _bits: u64,
    ) -> Result<(), crate::probe::DebugProbeError> {
        todo!()
    }

    fn swj_pins(
        &mut self,
        _pin_out: u32,
        _pin_select: u32,
        _pin_wait: u32,
    ) -> Result<u32, crate::probe::DebugProbeError> {
        todo!()
    }
}
impl MemoryInterface<ArmError> for MemoryAccessPort<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        todo!()
    }

    fn read_64(&mut self, _address: u64, _data: &mut [u64]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_32(&mut self, _address: u64, _data: &mut [u32]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_16(&mut self, _address: u64, _data: &mut [u16]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_8(&mut self, _address: u64, _data: &mut [u8]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_64(&mut self, _address: u64, _data: &[u64]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_32(&mut self, _address: u64, _data: &[u32]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_16(&mut self, _address: u64, _data: &[u16]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_8(&mut self, _address: u64, _data: &[u8]) -> Result<(), ArmError> {
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

    fn fully_qualified_address(&self) -> FullyQualifiedApAddress {
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
    ) -> Result<
        (
            &mut ArmCommunicationInterface<Initialized>,
            &mut super::ap_v1::memory_ap::MemoryAp,
        ),
        crate::probe::DebugProbeError,
    > {
        todo!()
    }
}

struct RootMemoryAP;

struct RootMemoryInterface<'iface> {
    iface: &'iface mut ACI,
    dp: DpAddress,
    base: u64,
}
impl<'iface> RootMemoryInterface<'iface> {
    fn new(iface: &'iface mut ACI, dp: DpAddress) -> Result<Self, ArmError> {
        let base_ptr0: BASEPTR0 = iface.read_dp_register(dp)?;
        let base_ptr1: BASEPTR1 = iface.read_dp_register(dp)?;
        let base = base_ptr0
            .valid()
            .then(|| u64::from(base_ptr1.ptr()) | u64::from(base_ptr0.ptr() << 12))
            .inspect(|base| tracing::info!("DPv3 BASE_PTR: 0x{base:x}"))
            .ok_or_else(|| ArmError::Other("DP has no valid base address defined.".into()))?;

        Ok(Self { iface, dp, base })
    }

    fn address(&self) -> FullyQualifiedApAddress {
        FullyQualifiedApAddress::v2_with_dp(self.dp, ApV2Address::Leaf(self.base))
    }

    fn base_address(&mut self) -> u64 {
        self.base
    }
}
impl SwdSequence for RootMemoryInterface<'_> {
    fn swj_sequence(
        &mut self,
        _bit_len: u8,
        _bits: u64,
    ) -> Result<(), crate::probe::DebugProbeError> {
        todo!()
    }

    fn swj_pins(
        &mut self,
        _pin_out: u32,
        _pin_select: u32,
        _pin_wait: u32,
    ) -> Result<u32, crate::probe::DebugProbeError> {
        todo!()
    }
}
impl MemoryInterface<ArmError> for RootMemoryInterface<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        todo!()
    }

    fn read_64(&mut self, _address: u64, _data: &mut [u64]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_32(&mut self, address: u64, _data: &mut [u32]) -> Result<(), ArmError> {
        let fq_address =
            FullyQualifiedApAddress::v2_with_dp(self.dp, ApV2Address::Leaf(self.base + address & 0xFFFF_FFFF_FFFF_FFF0));

        // read content
        for d in _data.iter_mut() {
            *d =
                self.iface.read_raw_ap_register(&fq_address, (address & 0xF) as u8)?;
        }
        Ok(())
    }

    fn read_16(&mut self, _address: u64, _data: &mut [u16]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_8(&mut self, _address: u64, _data: &mut [u8]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_64(&mut self, _address: u64, _data: &[u64]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_32(&mut self, _address: u64, _data: &[u32]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_16(&mut self, _address: u64, _data: &[u16]) -> Result<(), ArmError> {
        todo!()
    }

    fn write_8(&mut self, _address: u64, _data: &[u8]) -> Result<(), ArmError> {
        todo!()
    }

    fn supports_8bit_transfers(&self) -> Result<bool, ArmError> {
        todo!()
    }

    fn flush(&mut self) -> Result<(), ArmError> {
        todo!()
    }
}
impl ArmMemoryInterface for RootMemoryInterface<'_> {
    fn ap(&mut self) -> &mut super::ap_v1::memory_ap::MemoryAp {
        todo!()
    }

    fn fully_qualified_address(&self) -> FullyQualifiedApAddress {
        self.address()
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
    ) -> Result<
        (
            &mut ArmCommunicationInterface<Initialized>,
            &mut super::ap_v1::memory_ap::MemoryAp,
        ),
        crate::probe::DebugProbeError,
    > {
        todo!()
    }
}

pub fn enumerate_access_ports(
    probe: &mut ACI,
    dp: DpAddress,
) -> Result<BTreeSet<ApV2Address>, ArmError> {
    let mut root_ap = RootMemoryInterface::new(probe, dp)?;
    let base_address = root_ap.base_address();
    let rom_table = RomTable::try_parse(&mut root_ap, base_address)?;
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

/// Returns a Memory Interface accessing the Memory AP at the given `address` through the `iface`
/// Arm Communication Interface.
pub fn new_memory_interface<'i>(
    iface: &'i mut ACI,
    address: &FullyQualifiedApAddress,
) -> Result<Box<dyn ArmMemoryInterface + 'i>, ArmError> {
    let ApAddress::V2(ap_address) = address.ap() else {
        unimplemented!("this is only for APv2 addresses")
    };

    let mut next = None;
    let base = match ap_address {
        ApV2Address::Leaf(base) => base,
        ApV2Address::Node(base, n) => {
            next = Some(n);
            base
        }
    };
    let mut ap: Box<dyn ArmMemoryInterface + 'i> =
        Box::new(RootMemoryInterface::new(iface, address.dp())?);
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
