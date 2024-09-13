use crate::{
    architecture::arm::{
        communication_interface::{Initialized, SwdSequence},
        dp::{DpAccess, DpAddress, BASEPTR0, BASEPTR1},
        memory::ArmMemoryInterface,
        ApV2Address, ArmCommunicationInterface, ArmError, DapAccess, FullyQualifiedApAddress,
    },
    MemoryInterface,
};

type ACI = ArmCommunicationInterface<Initialized>;

pub struct RootMemoryInterface<'iface> {
    iface: &'iface mut ACI,
    dp: DpAddress,
    base: u64,
}
impl<'iface> RootMemoryInterface<'iface> {
    pub fn new(iface: &'iface mut ACI, dp: DpAddress) -> Result<Self, ArmError> {
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

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError> {
        //tracing::debug!("Reading {} words at {:x} on Root Access Port", data.len(), address);
        // read content
        for (i, d) in data.iter_mut().enumerate() {
            let addr = address + (i as u64) * 4;
            let base = (self.base + addr) & 0xFFFF_FFFF_FFFF_FFF0;
            let fqa = FullyQualifiedApAddress::v2_with_dp(self.dp, ApV2Address::new_with_tip(base));

            *d = self.iface.read_raw_ap_register(&fqa, (addr & 0xF) as u8)?;
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
    fn ap(&mut self) -> &mut crate::architecture::arm::ap_v1::memory_ap::MemoryAp {
        todo!()
    }

    fn fully_qualified_address(&self) -> FullyQualifiedApAddress {
        FullyQualifiedApAddress::v2_with_dp(self.dp, ApV2Address::new())
    }

    fn base_address(&mut self) -> Result<u64, ArmError> {
        Ok(self.base)
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
            &mut crate::architecture::arm::ap_v1::memory_ap::MemoryAp,
        ),
        crate::probe::DebugProbeError,
    > {
        todo!()
    }
}
