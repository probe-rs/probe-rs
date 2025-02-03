use crate::{
    architecture::arm::{
        ap_v2::registers::{DataSize, Register, CSW, DRW, TAR, TAR2},
        communication_interface::SwdSequence,
        memory::{ArmMemoryInterface, Status},
        ApAddress, ArmError, ArmProbeInterface, DapAccess, FullyQualifiedApAddress,
    },
    probe::DebugProbeError,
    MemoryInterface,
};

use super::registers::{BASE, BASE2};
use super::MaybeOwned;

pub struct MemoryAccessPortInterface<'iface> {
    iface: MaybeOwned<'iface>,
    base: u64,
}
impl<'iface> MemoryAccessPortInterface<'iface> {
    /// creates a new `MemoryAccessPortInterface` from a reference to a `dyn ArmMemoryInterface`.
    pub fn new_with_ref(
        iface: &'iface mut (dyn ArmMemoryInterface + 'iface),
        base: u64,
    ) -> Result<Self, ArmError> {
        // TODO! validity check from the parent root table
        Ok(Self {
            iface: MaybeOwned::Reference(iface),
            base,
        })
    }

    /// creates a new `MemoryAccessPortInterface` from a boxed `dyn ArmMemoryInterface`.
    pub fn boxed(iface: Box<dyn ArmMemoryInterface + 'iface>, base: u64) -> Result<Self, ArmError> {
        // TODO! validity check from the parent root table
        Ok(Self {
            iface: MaybeOwned::Boxed(iface),
            base,
        })
    }

    fn set_transaction_size(&mut self, size: DataSize) -> Result<(), ArmError> {
        let mut csw_raw = [0u32];
        self.iface
            .read_32(self.base + u64::from(CSW::ADDRESS), &mut csw_raw)?;
        let mut csw = CSW::try_from(csw_raw[0])?;
        csw.SIZE = size;
        csw.DbgSwEnable = true;

        // TODO: This is setting the HPROT field to 0b10 and the HNONSEC = 1 for the AHB5 CSW. This
        // should be moved into a bus-specific CSW control access.
        csw.Prot = (1 << (25 - 24)) | (1 << (29 - 24));

        self.iface
            .write_word_32(self.base + u64::from(CSW::ADDRESS), u32::from(csw))?;
        Ok(())
    }

    fn set_address(&mut self, address: u64) -> Result<(), ArmError> {
        self.iface
            .write_word_32(self.base + u64::from(TAR::ADDRESS), address as u32)?;
        self.iface.flush()?;
        self.iface
            .write_word_32(self.base + u64::from(TAR2::ADDRESS), (address >> 32) as u32)?;
        self.iface.flush()?;
        Ok(())
    }
}

impl MemoryInterface<ArmError> for MemoryAccessPortInterface<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        false
    }

    fn read_64(&mut self, _address: u64, _data: &mut [u64]) -> Result<(), ArmError> {
        unimplemented!()
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError> {
        self.set_transaction_size(DataSize::U32)?;

        // iface: fully qualified address points parent
        // base-address: base for the registers of this AP in the parent’s memory space
        // address: register address of the register, relative to the base address.
        for (i, d) in data.iter_mut().enumerate() {
            let address = address + (i as u64) * 4;
            self.set_address(address)?;
            *d = self
                .iface
                .read_word_32(self.base + u64::from(DRW::ADDRESS))?;
        }

        Ok(())
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), ArmError> {
        if (address % 2) != 0 {
            return Err(ArmError::alignment_error(address, 2));
        }

        self.set_transaction_size(DataSize::U16)?;

        // iface: fully qualified address points parent
        // base-address: base for the registers of this AP in the parent’s memory space
        // address: register address of the register, relative to the base address.
        for (i, d) in data.iter_mut().enumerate() {
            let address = address + (i as u64) * 2;
            self.set_address(address)?;
            let drw = self
                .iface
                .read_word_32(self.base + u64::from(DRW::ADDRESS))?;

            // According to ARM Debug Interface Architecture Specification ADIv6.0
            // "C2.2.7 Byte lanes" the contents of DRW need to be shifted appropriately.
            *d = ((drw >> ((address % 4) * 8)) & 0xFFFF) as u16;
        }

        Ok(())
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), ArmError> {
        self.set_transaction_size(DataSize::U8)?;

        // iface: fully qualified address points parent
        // base-address: base for the registers of this AP in the parent’s memory space
        // address: register address of the register, relative to the base address.
        for (i, d) in data.iter_mut().enumerate() {
            let address = address + (i as u64);
            self.set_address(address)?;
            let drw = self
                .iface
                .read_word_32(self.base + u64::from(DRW::ADDRESS))?;

            // According to ARM Debug Interface Architecture Specification ADIv6.0
            // "C2.2.7 Byte lanes" the contents of DRW need to be shifted appropriately.
            *d = ((drw >> ((address % 4) * 8)) & 0xFF) as u8;
        }

        Ok(())
    }

    fn write_64(&mut self, _address: u64, _data: &[u64]) -> Result<(), ArmError> {
        unimplemented!()
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), ArmError> {
        self.set_transaction_size(DataSize::U32)?;
        // iface: fully qualified address points parent
        // base-address: base for the registers of this AP in the parent’s memory space
        // address: register address of the register, relative to the base address.
        for (i, d) in data.iter().enumerate() {
            let address = address + (i as u64) * 4;
            self.set_address(address)?;
            self.iface
                .write_word_32(self.base + u64::from(DRW::ADDRESS), *d)?;
        }

        Ok(())
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), ArmError> {
        self.set_transaction_size(DataSize::U16)?;
        // iface: fully qualified address points parent
        // base-address: base for the registers of this AP in the parent’s memory space
        // address: register address of the register, relative to the base address.
        for (i, d) in data.iter().enumerate() {
            let address = address + (i as u64) * 2;
            self.set_address(address)?;
            self.iface
                .write_word_32(self.base + u64::from(DRW::ADDRESS), *d as u32)?;
        }

        Ok(())
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), ArmError> {
        self.set_transaction_size(DataSize::U8)?;
        // iface: fully qualified address points parent
        // base-address: base for the registers of this AP in the parent’s memory space
        // address: register address of the register, relative to the base address.
        for (i, d) in data.iter().enumerate() {
            let address = address + (i as u64);
            self.set_address(address)?;
            self.iface
                .write_word_32(self.base + u64::from(DRW::ADDRESS), *d as u32)?;
        }

        Ok(())
    }

    fn supports_8bit_transfers(&self) -> Result<bool, ArmError> {
        Ok(true)
    }

    fn flush(&mut self) -> Result<(), ArmError> {
        self.iface.flush()
    }
}
impl ArmMemoryInterface for MemoryAccessPortInterface<'_> {
    fn fully_qualified_address(&self) -> FullyQualifiedApAddress {
        let (dp, ApAddress::V2(ap)) = self.iface.fully_qualified_address().deconstruct() else {
            panic!("The sub-interface returned an address with an unexpected version. This is a bug, please report it.")
        };
        FullyQualifiedApAddress::v2_with_dp(dp, ap.append(self.base))
    }

    fn base_address(&mut self) -> Result<u64, ArmError> {
        let mut base = 0;
        let mut base1 = 0;
        self.iface.read_32(
            self.base + u64::from(BASE::ADDRESS),
            std::slice::from_mut(&mut base),
        )?;
        self.iface.read_32(
            self.base + u64::from(BASE2::ADDRESS),
            std::slice::from_mut(&mut base1),
        )?;
        let base = (u64::from(base1) << 32) | u64::from(base);
        tracing::debug!(
            "{:x?}’s rom table is at: {:x}",
            self.fully_qualified_address(),
            base
        );
        Ok(base & 0xFFFF_FFFF_FFFF_FFF0)
    }

    fn get_swd_sequence(&mut self) -> Result<&mut dyn SwdSequence, DebugProbeError> {
        self.iface.get_swd_sequence()
    }

    fn get_arm_probe_interface(&mut self) -> Result<&mut dyn ArmProbeInterface, DebugProbeError> {
        self.iface.get_arm_probe_interface()
    }

    fn get_dap_access(&mut self) -> Result<&mut dyn DapAccess, DebugProbeError> {
        self.iface.get_dap_access()
    }

    fn generic_status(&mut self) -> Result<Status, ArmError> {
        let mut csw_raw = [0u32];
        self.iface
            .read_32(self.base + u64::from(CSW::ADDRESS), &mut csw_raw)?;
        let csw = CSW::try_from(csw_raw[0])?;
        Ok(Status::V2(csw))
    }
}
