#![allow(non_snake_case)]

use crate::architecture::arm::{
    ArmError, DapAccess, FullyQualifiedApAddress,
    ap::{AccessPortType, ApAccess, ApRegAccess, ApRegister, CFG, define_ap_register},
};

use super::{AddressIncrement, DataSize};

/// Memory AP
///
/// The memory AP can be used to access a memory-mapped
/// set of debug resources of the attached system.
#[derive(Debug)]
pub struct AmbaAxi3Axi4 {
    address: FullyQualifiedApAddress,
    csw: CSW,
    cfg: CFG,
}

impl AmbaAxi3Axi4 {
    /// Creates a new AmbaAhb5Hprot with `address` as base address.
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
        csw.set_Privileged(true);
        csw.set_AddrInc(AddressIncrement::Single);
        probe.write_ap_register(&me, csw)?;
        Ok(Self { csw, ..me })
    }
}

impl super::MemoryApType for AmbaAxi3Axi4 {
    type CSW = CSW;

    fn status<P: ApAccess + ?Sized>(&mut self, probe: &mut P) -> Result<CSW, ArmError> {
        const { assert!(crate::architecture::arm::ap::CSW::ADDRESS == CSW::ADDRESS) };
        self.csw = probe.read_ap_register(self)?;
        Ok(self.csw)
    }

    fn try_set_datasize<P: ApAccess + ?Sized>(
        &mut self,
        probe: &mut P,
        data_size: DataSize,
    ) -> Result<(), ArmError> {
        match data_size {
            DataSize::U8 | DataSize::U16 | DataSize::U32 if data_size != self.csw.Size() => {
                let mut csw = self.csw;
                csw.set_Size(data_size);
                probe.write_ap_register(self, csw)?;
                self.csw = csw;
            }
            DataSize::U64 | DataSize::U128 | DataSize::U256 if data_size != self.csw.Size() => {
                if !self.has_large_data_extension() {
                    return Err(ArmError::UnsupportedTransferWidth(
                        data_size.to_byte_count() * 8,
                    ));
                }

                let mut csw = self.csw;
                csw.set_Size(data_size);

                probe.write_ap_register(self, csw)?;

                // MEM-AP Large Data Extension:
                // "If the CSW.Size field is written with a value corresponding to a size that is not
                // supported, or with a reserved value:
                // • A read of the field returns a value corresponding to a supported size."
                //
                // So read back CSW to see if the extended size is actually supported.

                self.csw = probe.read_ap_register(self)?;

                if csw.Size() != self.csw.Size() {
                    return Err(ArmError::UnsupportedTransferWidth(
                        data_size.to_byte_count() * 8,
                    ));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn has_large_address_extension(&self) -> bool {
        self.cfg.LA()
    }

    fn has_large_data_extension(&self) -> bool {
        self.cfg.LD()
    }

    fn supports_only_32bit_data_size(&self) -> bool {
        // Amba AHB5 must support word, half-word and byte size transfers.
        false
    }
}

impl AccessPortType for AmbaAxi3Axi4 {
    fn ap_address(&self) -> &FullyQualifiedApAddress {
        &self.address
    }
}

impl ApRegAccess<CSW> for AmbaAxi3Axi4 {}

super::attached_regs_to_mem_ap!(memory_ap_regs => AmbaAxi3Axi4);

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
        pub Instruction, set_Instruction: 30;
        /// Is the transaction request non-secure
        ///
        /// - If 1 a non-secure transfer is initiated.
        /// - If 0 and SPIDEN is 1 then a secure transfer is initiated.
        /// - If 0 and SPIDEN is 0 then no transaction is initiated.
        pub NonSecure, set_NonSecure: 29;
        /// Is this transaction privileged
        pub Privileged, set_Privileged: 28;
        /// Drives `AxCACHE[3:0]` where x is R for reads and W for writes.
        /// Amba AXI4 requires asymmetrical usage of `ARCACHE` and `AWCACHE`.
        pub u8, CACHE, set_CACHE: 27, 24;
        /// Secure Debug Enabled.
        ///
        /// This bit reflects the state of the CoreSight authentication signal.
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
        pub u8, from into DataSize, Size, set_Size: 2, 0;
    ]
);
