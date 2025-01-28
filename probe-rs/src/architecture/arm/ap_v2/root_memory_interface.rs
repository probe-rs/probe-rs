//! Functions to access the root memory interface of the Debug Port (DP) in an ADIv6
//! implementation.
use crate::{
    architecture::arm::{
        communication_interface::SwdSequence,
        dp::{DpAccess, DpAddress, BASEPTR0, BASEPTR1},
        memory::ArmMemoryInterface,
        ApV2Address, ArmError, ArmProbeInterface, DapAccess, FullyQualifiedApAddress,
    },
    probe::DebugProbeError,
    MemoryInterface,
};

/// The Root Memory Interface accesses the Debug Port (DP) address space. This memory interface can
/// only be used to interface into the ROM tables and CoreSight components of the debug
/// infrastructure.
pub struct RootMemoryInterface<'iface, API> {
    iface: &'iface mut API,
    dp: DpAddress,
    base: u64,
}
impl<'iface, API: ArmProbeInterface> RootMemoryInterface<'iface, API> {
    pub fn new(iface: &'iface mut API, dp: DpAddress) -> Result<Self, ArmError> {
        let base_ptr0: BASEPTR0 = iface.read_dp_register(dp)?;
        let base_ptr1: BASEPTR1 = iface.read_dp_register(dp)?;
        let base = base_ptr0
            .valid()
            .then(|| u64::from(base_ptr1.ptr()) | u64::from(base_ptr0.ptr() << 12))
            .inspect(|base| tracing::info!("DPv3 BASE_PTR: 0x{base:x}"))
            .ok_or_else(|| ArmError::Other("DP has no valid base address defined.".into()))?;

        Ok(Self { iface, dp, base })
    }
}

impl<'iface, API: ArmProbeInterface> MemoryInterface<ArmError>
    for RootMemoryInterface<'iface, API>
{
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
            let base = addr & (!0xF);
            let fqa = FullyQualifiedApAddress::v2_with_dp(self.dp, ApV2Address::new_with_tip(base));

            *d = self.iface.read_raw_ap_register(&fqa, (addr & 0xF) as u8)?;
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
            let base = (self.base + addr) & 0xFFFF_FFFF_FFFF_FFF0;
            let fqa = FullyQualifiedApAddress::v2_with_dp(self.dp, ApV2Address::new_with_tip(base));

            self.iface
                .write_raw_ap_register(&fqa, (addr & 0xF) as u8, *d)?;
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
impl<'iface, API: ArmProbeInterface> ArmMemoryInterface for RootMemoryInterface<'iface, API> {
    fn fully_qualified_address(&self) -> FullyQualifiedApAddress {
        FullyQualifiedApAddress::v2_with_dp(self.dp, ApV2Address::root())
    }

    fn base_address(&mut self) -> Result<u64, ArmError> {
        Ok(self.base)
    }

    fn get_swd_sequence(&mut self) -> Result<&mut dyn SwdSequence, DebugProbeError> {
        Ok(self.iface)
    }

    fn get_arm_probe_interface(&mut self) -> Result<&mut dyn ArmProbeInterface, DebugProbeError> {
        Ok(self.iface)
    }

    fn get_dap_access(&mut self) -> Result<&mut dyn DapAccess, DebugProbeError> {
        Ok(self.iface)
    }

    fn generic_status(&mut self) -> Result<crate::architecture::arm::memory::Status, ArmError> {
        // This is not a memory AP, so there's no logicl CSW associated with it.
        unimplemented!()
    }
}
