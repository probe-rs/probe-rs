//! Functions to access the root memory interface of the Debug Port (DP) in an ADIv6
//! implementation.
use crate::{
    MemoryInterface,
    architecture::arm::{
        ApV2Address, ArmDebugInterface, ArmError, FullyQualifiedApAddress,
        dp::{BASEPTR0, BASEPTR1, DpAccess, DpAddress},
        memory::ArmMemoryInterface,
    },
    probe::DebugProbeError,
};

/// The Root Memory Interface accesses the Debug Port (DP) address space. This memory interface can
/// only be used to interface into the ROM tables and CoreSight components of the debug
/// infrastructure.
pub struct RootMemoryInterface<'iface, API> {
    iface: &'iface mut API,
    dp: DpAddress,
}
impl<'iface, ADI: ArmDebugInterface> RootMemoryInterface<'iface, ADI> {
    pub fn new(iface: &'iface mut ADI, dp: DpAddress) -> Result<Self, ArmError> {
        Ok(Self { iface, dp })
    }
}

impl<API: ArmDebugInterface> MemoryInterface<ArmError> for RootMemoryInterface<'_, API> {
    fn supports_native_64bit_access(&mut self) -> bool {
        false
    }

    fn read_64(&mut self, _address: u64, _data: &mut [u64]) -> Result<(), ArmError> {
        unimplemented!("The DPv3 only supports 32bit accesses")
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError> {
        //tracing::debug!("Reading {} words at {:x} on Root Access Port", data.len(), address);
        // read content
        for (i, d) in data.iter_mut().enumerate() {
            let addr = address + (i as u64) * 4;
            let fqa = FullyQualifiedApAddress::v2_with_dp(self.dp, ApV2Address::root());
            *d = self.iface.read_raw_ap_register(&fqa, addr)?;
        }
        Ok(())
    }

    fn read_16(&mut self, _address: u64, _data: &mut [u16]) -> Result<(), ArmError> {
        unimplemented!("The DPv3 only supports 32bit accesses")
    }

    fn read_8(&mut self, _address: u64, _data: &mut [u8]) -> Result<(), ArmError> {
        unimplemented!("The DPv3 only supports 32bit accesses")
    }

    fn write_64(&mut self, _address: u64, _data: &[u64]) -> Result<(), ArmError> {
        unimplemented!("The DPv3 only supports 32bit accesses")
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), ArmError> {
        // read content
        for (i, d) in data.iter().enumerate() {
            let addr = address + (i as u64) * 4;
            let fqa = FullyQualifiedApAddress::v2_with_dp(self.dp, ApV2Address::root());
            self.iface.write_raw_ap_register(&fqa, addr, *d)?;
        }
        Ok(())
    }

    fn write_16(&mut self, _address: u64, _data: &[u16]) -> Result<(), ArmError> {
        unimplemented!("The DPv3 only supports 32bit accesses")
    }

    fn write_8(&mut self, _address: u64, _data: &[u8]) -> Result<(), ArmError> {
        unimplemented!("The DPv3 only supports 32bit accesses")
    }

    fn supports_8bit_transfers(&self) -> Result<bool, ArmError> {
        Ok(false)
    }

    fn flush(&mut self) -> Result<(), ArmError> {
        // transfers are not buffered.
        Ok(())
    }
}
impl<API: ArmDebugInterface> ArmMemoryInterface for RootMemoryInterface<'_, API> {
    fn fully_qualified_address(&self) -> FullyQualifiedApAddress {
        FullyQualifiedApAddress::v2_with_dp(self.dp, ApV2Address::root())
    }

    fn base_address(&mut self) -> Result<u64, ArmError> {
        let base_ptr0: BASEPTR0 = self.iface.read_dp_register(self.dp)?;
        let base_ptr1: BASEPTR1 = self.iface.read_dp_register(self.dp)?;
        let base = base_ptr0
            .valid()
            .then(|| u64::from(base_ptr1.ptr()) | u64::from(base_ptr0.ptr() << 12))
            .ok_or_else(|| ArmError::Other("DP has no valid base address defined.".into()))?;

        Ok(base)
    }

    fn get_arm_debug_interface(&mut self) -> Result<&mut dyn ArmDebugInterface, DebugProbeError> {
        Ok(self.iface)
    }

    fn generic_status(&mut self) -> Result<crate::architecture::arm::ap::CSW, ArmError> {
        // This is not a memory AP, so there's no logicl CSW associated with it.
        unimplemented!()
    }
}
