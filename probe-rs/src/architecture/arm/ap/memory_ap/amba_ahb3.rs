use crate::architecture::arm::{
    ArmError, DapAccess, FullyQualifiedApAddress, RegisterParseError,
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
    pub async fn new<P: DapAccess>(
        probe: &mut P,
        address: FullyQualifiedApAddress,
    ) -> Result<Self, ArmError> {
        let csw = probe.read_raw_ap_register(&address, CSW::ADDRESS).await?;
        let cfg = probe.read_raw_ap_register(&address, CFG::ADDRESS).await?;
        let (csw, cfg) = (csw.try_into()?, cfg.try_into()?);

        let me = Self { address, csw, cfg };
        let csw = CSW {
            DbgSwEnable: true,
            HNONSEC: !csw.SPIDEN,
            MasterType: true,
            Cacheable: true,
            Privileged: true,
            Data: true,
            AddrInc: AddressIncrement::Single,
            ..me.csw
        };
        probe.write_ap_register(&me, csw).await?;
        Ok(Self { csw, ..me })
    }
}

#[async_trait::async_trait(?Send)]
impl super::MemoryApType for AmbaAhb3 {
    type CSW = CSW;

    async fn status<P: ApAccess + ?Sized>(&mut self, probe: &mut P) -> Result<CSW, ArmError> {
        const { assert!(crate::architecture::arm::ap::CSW::ADDRESS == CSW::ADDRESS) };
        self.csw = probe.read_ap_register(self).await?;
        Ok(self.csw)
    }

    async fn try_set_datasize<P: ApAccess + ?Sized>(
        &mut self,
        probe: &mut P,
        data_size: DataSize,
    ) -> Result<(), ArmError> {
        match data_size {
            DataSize::U8 | DataSize::U16 | DataSize::U32 if data_size != self.csw.Size => {
                let csw = CSW {
                    Size: data_size,
                    ..self.csw
                };
                probe.write_ap_register(self, csw).await?;
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
        self.cfg.LA
    }

    fn has_large_data_extension(&self) -> bool {
        self.cfg.LD
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
        DbgSwEnable: bool,          // [31]
        /// HNONSEC
        ///
        /// Not formally defined.
        /// If implemented should be 1 at reset.
        /// If not implemented, should be 1 and writing 0 leads to unpredictable AHB-AP behavior.
        HNONSEC: bool,              // [30]
        /// Defines which Requester ID is used on `HMASTER[3:0]` signals.
        ///
        /// Support of this function is implementation defined.
        MasterType: bool,           // [29]
        /// Drives `HPROT[4]`, Allocate.
        ///
        /// `HPROT[4]` is an Armv5 extension to AHB. For more information, see the Arm1136JF-S™ and
        /// Arm1136J-S ™ Technical Reference Manual.
        Allocate: bool,             // [28]
        /// `HPROT[3]`
        Cacheable: bool,            // [27]
        /// `HPROT[2]`
        Bufferable: bool,           // [26]
        /// `HPROT[1]`
        Privileged: bool,           // [25]
        /// `HPROT[0]`
        Data: bool,                 // [24]
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
        SPIDEN: bool,               // [23]
        /// A transfer is in progress.
        /// Can be used to poll whether an aborted transaction has completed.
        /// Read only.
        TrInProg: bool,             // [7]
        /// `1` if transactions can be issued through this access port at the moment.
        /// Read only.
        DeviceEn: bool,             // [6]
        /// The address increment on DRW access.
        AddrInc: AddressIncrement,  // [5:4]
        /// The access size of this memory AP.
        Size: DataSize,             // [2:0]
        /// Reserved bit, kept to preserve IMPLEMENTATION DEFINED statuses.
        _reserved_bits: u32         // mask
    ],
    from: value => Ok(CSW {
        DbgSwEnable: ((value >> 31) & 0x01) != 0,
        HNONSEC:    ((value >> 30) & 0x01) != 0,
        MasterType: ((value >> 29) & 0x01) != 0,
        Allocate:   ((value >> 28) & 0x01) != 0,
        Cacheable:  ((value >> 27) & 0x01) != 0,
        Bufferable: ((value >> 26) & 0x01) != 0,
        Privileged: ((value >> 25) & 0x01) != 0,
        Data:       ((value >> 24) & 0x01) != 0,
        SPIDEN:     ((value >> 23) & 0x01) != 0,
        TrInProg:   ((value >> 7) & 0x01) != 0,
        DeviceEn:   ((value >> 6) & 0x01) != 0,
        AddrInc: AddressIncrement::from_u8(((value >> 4) & 0x03) as u8).ok_or_else(|| RegisterParseError::new("CSW", value))?,
        Size: DataSize::try_from((value & 0x07) as u8).map_err(|_| RegisterParseError::new("CSW", value))?,
        _reserved_bits: value & 0x007F_FF08,
    }),
    to: value => (u32::from(value.DbgSwEnable) << 31)
    | (u32::from(value.HNONSEC      ) << 30)
    | (u32::from(value.MasterType   ) << 29)
    | (u32::from(value.Allocate     ) << 28)
    | (u32::from(value.Cacheable    ) << 27)
    | (u32::from(value.Bufferable   ) << 26)
    | (u32::from(value.Privileged   ) << 25)
    | (u32::from(value.Data         ) << 24)
    | (u32::from(value.SPIDEN       ) << 23)
    | (u32::from(value.TrInProg     ) <<  7)
    | (u32::from(value.DeviceEn     ) <<  6)
    | (u32::from(value.AddrInc as u8) <<  4)
    | (value.Size as u32)
    | value._reserved_bits
);
