use std::any::Any;

use crate::{
    architecture::arm::{
        ap::{memory::{registers::{BASE, BASE2, DRW, TAR, TAR2}, MemoryAp}, v2::Register},
        communication_interface::{Initialized, SwdSequence},
        memory::ArmMemoryInterface,
        ApAddress, ArmCommunicationInterface, ArmError, FullyQualifiedApAddress,
    },
    MemoryInterface,
};

use super::MaybeOwned;

pub struct MemoryAccessPortInterface<'iface> {
    iface: MaybeOwned<'iface>,
    //memory_ap: MemoryAp,
    base: u64,
}
impl<'iface> MemoryAccessPortInterface<'iface> {
    pub fn new<M: ArmMemoryInterface + 'iface>(iface: M, base: u64) -> Result<Self, ArmError> {
        //MemoryAp::new(interface, address);
        //
        // TODO! validity check from the parent root table
        Ok(Self {
            iface: MaybeOwned::Boxed(Box::new(iface)),
            base,
        })
    }

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
}

impl<'iface> SwdSequence for MemoryAccessPortInterface<'iface> {
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
impl<'iface> MemoryInterface<ArmError> for MemoryAccessPortInterface<'iface> {
    fn supports_native_64bit_access(&mut self) -> bool {
        todo!()
    }

    fn read_64(&mut self, _address: u64, _data: &mut [u64]) -> Result<(), ArmError> {
        todo!()
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError> {
        // iface: fully qualified address points parent
        // base-address: base for the registers of this AP in the parent’s memory space
        // address: register address of the register, relative to the base address.
        let _faq = self.fully_qualified_address();
        for (i, d) in data.iter_mut().enumerate() {
            let address = address + (i as u64) * 4;
            //tracing::debug!("Setting TAR to : {address:x}.");
            self.iface
                .write_word_32(self.base + u64::from(TAR::ADDRESS), address as u32)?;
            self.iface
                .write_word_32(self.base + u64::from(TAR2::ADDRESS), (address >> 32) as u32)?;
            *d = self
                .iface
                .read_word_32(self.base + u64::from(DRW::ADDRESS))?;
            //tracing::trace!("Reading at {:x?}->{:x}: {:x}", _faq, address, d);
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
        // iface: fully qualified address points parent
        // base-address: base for the registers of this AP in the parent’s memory space
        // address: register address of the register, relative to the base address.
        let _faq = self.fully_qualified_address();
        for (i, d) in data.iter().enumerate() {
            let address = address + (i as u64) * 4;
            //tracing::debug!("Setting TAR to : {address:x}.");
            self.iface
                .write_word_32(self.base + u64::from(TAR::ADDRESS), address as u32)?;
            self.iface
                .write_word_32(self.base + u64::from(TAR2::ADDRESS), (address >> 32) as u32)?;
            self.iface
                .write_word_32(self.base + u64::from(DRW::ADDRESS), *d)?;
            //tracing::trace!("Reading at {:x?}->{:x}: {:x}", _faq, address, d);
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
impl<'iface> ArmMemoryInterface for MemoryAccessPortInterface<'iface> {
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

    fn get_arm_communication_interface(
        &mut self,
    ) -> Result<&mut ArmCommunicationInterface<Initialized>, crate::probe::DebugProbeError> {
        self.iface.get_arm_communication_interface()
    }

    fn try_as_parts(
        &mut self,
    ) -> Result<
        (
            &mut ArmCommunicationInterface<Initialized>,
            &mut crate::architecture::arm::ap::memory::MemoryAp,
        ),
        crate::probe::DebugProbeError,
    > {
        todo!()
    }
}
