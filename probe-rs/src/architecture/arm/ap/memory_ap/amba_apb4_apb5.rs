#![allow(non_snake_case)]

use crate::architecture::arm::{
    ArmError, DapAccess, FullyQualifiedApAddress,
    ap::{
        AccessPortType, AddressIncrement, ApAccess, ApRegAccess, ApRegister, CFG, DataSize,
        define_ap_register,
    },
};

/// Memory AP
///
/// The memory AP can be used to access a memory-mapped
/// set of debug resources of the attached system.
#[derive(Debug)]
pub struct AmbaApb4Apb5 {
    address: FullyQualifiedApAddress,
    csw: CSW,
    cfg: CFG,
}

impl AmbaApb4Apb5 {
    /// Creates a new AmbaAhb3 with `address` as base address.
    pub fn new<P: DapAccess>(
        probe: &mut P,
        address: FullyQualifiedApAddress,
    ) -> Result<Self, ArmError> {
        let csw = probe.read_raw_ap_register(&address, CSW::ADDRESS)?;
        let cfg = probe.read_raw_ap_register(&address, CFG::ADDRESS)?;

        let (csw, cfg) = (csw.try_into()?, cfg.try_into()?);

        let me = Self { address, csw, cfg };
        let mut csw = me.csw;
        csw.set_DbgSwEnable(true);
        csw.set_AddrInc(AddressIncrement::Single);
        probe.write_ap_register(&me, csw)?;
        Ok(Self { csw, ..me })
    }
}

impl super::MemoryApType for AmbaApb4Apb5 {
    type CSW = CSW;

    fn status<P: ApAccess + ?Sized>(&mut self, probe: &mut P) -> Result<CSW, ArmError> {
        const { assert!(crate::architecture::arm::ap::CSW::ADDRESS == CSW::ADDRESS) };
        self.csw = probe.read_ap_register(self)?;
        Ok(self.csw)
    }

    fn try_set_datasize<P: ApAccess + ?Sized>(
        &mut self,
        _probe: &mut P,
        data_size: DataSize,
    ) -> Result<(), ArmError> {
        match data_size {
            DataSize::U32 => Ok(()),
            _ => Err(ArmError::UnsupportedTransferWidth(
                data_size.to_byte_count() * 8,
            )),
        }
    }

    fn has_large_address_extension(&self) -> bool {
        self.cfg.LA()
    }

    fn has_large_data_extension(&self) -> bool {
        self.cfg.LD()
    }

    fn supports_only_32bit_data_size(&self) -> bool {
        // APB4 and APB5 AP only support 32bit accesses
        true
    }
}

impl AccessPortType for AmbaApb4Apb5 {
    fn ap_address(&self) -> &FullyQualifiedApAddress {
        &self.address
    }
}

impl ApRegAccess<CSW> for AmbaApb4Apb5 {}

super::attached_regs_to_mem_ap!(memory_ap_regs => AmbaApb4Apb5);

define_ap_register!(
    /// Control and Status Word register
    ///
    /// The control and status word register (CSW) is used
    /// to configure memory access through the memory AP.
    name: CSW,
    address: 0x00,
    fields: [
        /// Is debug software access enabled.
        pub DbgSwEnable, set_DbgSwEnable: 31;
        /// Is a Non-secure transfer requested.
        /// If a secure transfer is requested, the behaviour depends on the value of SPIDEN.
        /// - If SPIDEN is 1 then a secure transfer is initiated.
        /// - IF SPIDEN is 0, then no transfer is initiated. An access to DRW or BD0-BD3 is likely
        ///   to return an error.
        pub NonSecure, set_NonSecure: 29;
        /// Is this transaction privileged
        pub Privileged, set_Privileged: 28;
        /// May reflect the state of the CoreSight authentication interface.
        /// If Secure debug is not supported, this field is always 0.
        pub SPIDEN, set_SPIDEN: 23;
        /// A transfer is in progress.
        /// Can be used to poll whether an aborted transaction has completed.
        /// Read only.
        pub TrInProg, set_TrInProg: 7;
        /// `1` if transactions can be issued through this access port at the moment.
        /// Read only.
        pub DeviceEn, set_DeviceEn: 6;
        /// The address increment on DRW access.
        pub u8, from into AddressIncrement, AddrInc, set_AddrInc: 5, 4;
        /// The access size of this memory AP.
        /// Only supports word accesses.
        pub u8, from into DataSize, Size, set_Size: 2, 0;
    ]
);
