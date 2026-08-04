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
pub struct AmbaAhb3 {
    address: FullyQualifiedApAddress,
    csw: CSW,
    cfg: CFG,
}

impl AmbaAhb3 {
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
        csw.set_HNONSEC(!csw.SPIDEN());
        csw.set_MasterType(true);
        csw.set_Cacheable(true);
        csw.set_Privileged(true);
        csw.set_Data(true);
        csw.set_AddrInc(AddressIncrement::Single);
        probe.write_ap_register(&me, csw)?;
        Ok(Self { csw, ..me })
    }
}

impl super::MemoryApType for AmbaAhb3 {
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
            DataSize::U64 | DataSize::U128 | DataSize::U256 => {
                return Err(ArmError::UnsupportedTransferWidth(
                    data_size.to_byte_count() * 8,
                ));
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
        // Amba AHB3 must support word, half-word and byte size transfers.
        false
    }
}

impl AccessPortType for AmbaAhb3 {
    fn ap_address(&self) -> &FullyQualifiedApAddress {
        &self.address
    }
}

impl ApRegAccess<CSW> for AmbaAhb3 {}

super::attached_regs_to_mem_ap!(memory_ap_regs => AmbaAhb3);

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
        /// HNONSEC
        ///
        /// Not formally defined.
        /// If implemented should be 1 at reset.
        /// If not implemented, should be 1 and writing 0 leads to unpredictable AHB-AP behavior.
        pub HNONSEC, set_HNONSEC: 30;
        /// Defines which Requester ID is used on `HMASTER[3:0]` signals.
        ///
        /// Support of this function is implementation defined.
        pub MasterType, set_MasterType: 29;
        /// Drives `HPROT[4]`, Allocate.
        ///
        /// `HPROT[4]` is an Armv5 extension to AHB. For more information, see the Arm1136JF-S™ and
        /// Arm1136J-S ™ Technical Reference Manual.
        pub Allocate, set_Allocate: 28;
        /// `HPROT[3]`
        pub Cacheable, set_Cacheable: 27;
        /// `HPROT[2]`
        pub Bufferable, set_Bufferable: 26;
        /// `HPROT[1]`
        pub Privileged, set_Privileged: 25;
        /// `HPROT[0]`
        pub Data, set_Data: 24;
        /// Secure Debug Enabled.
        ///
        /// This field has one of the following values:
        /// - `0b0` Secure access is disabled.
        /// - `0b1` Secure access is enabled.
        /// This field is optional, and read-only. If not implemented, the bit is RES0.
        /// If CSW.DeviceEn is 0b0, SPIDEN is ignored and the effective value of SPIDEN is 0b1.
        /// For more information, see `Enabling access to the connected debug device or memory system`
        /// on page C2-154.
        ///
        /// Note:
        /// In ADIv5 and older versions of the architecture, the CSW.SPIDEN field is in the same bit
        /// position as CSW.SDeviceEn, and has the same meaning. From ADIv6, the name SDeviceEn is
        /// used to avoid confusion between this field and the SPIDEN signal on the authentication
        /// interface.
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
