use crate::architecture::arm::{
    ap::{define_ap_register, ApRegister, CFG},
    ap_v1::{AccessPortType, ApAccess, ApRegAccess},
    ArmError, DapAccess, FullyQualifiedApAddress, RegisterParseError,
};

use super::{AddressIncrement, DataSize};

/// Memory AP
///
/// The memory AP can be used to access a memory-mapped
/// set of debug resources of the attached system.
#[derive(Debug)]
pub struct AmbaAxi5 {
    address: FullyQualifiedApAddress,
    csw: CSW,
    cfg: CFG,
}

impl AmbaAxi5 {
    /// Creates a new AmbaAhb5Hprot with `address` as base address.
    pub fn new<P: DapAccess>(
        probe: &mut P,
        address: FullyQualifiedApAddress,
    ) -> Result<Self, ArmError> {
        let csw = probe.read_raw_ap_register(&address, CSW::ADDRESS)?;
        let cfg = probe.read_raw_ap_register(&address, CFG::ADDRESS)?;
        let (csw, cfg) = (csw.try_into()?, cfg.try_into()?);

        let me = Self { address, csw, cfg };
        let csw = CSW {
            DbgSwEnable: true,
            Privileged: true,
            AddrInc: AddressIncrement::Single,
            ..me.csw
        };
        probe.write_ap_register(&me, csw)?;
        Ok(Self { csw, ..me })
    }
}

impl super::MemoryApType for AmbaAxi5 {
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
            DataSize::U8 | DataSize::U16 | DataSize::U32 if data_size != self.csw.Size => {
                let csw = CSW {
                    Size: data_size,
                    ..self.csw
                };
                probe.write_ap_register(self, csw)?;
                self.csw = csw;
            }
            DataSize::U64 | DataSize::U128 | DataSize::U256 => {
                return Err(ArmError::UnsupportedTransferWidth(
                    data_size.to_byte_count() * 8,
                ))
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
        // Amba AHB5 must support word, half-word and byte size transfers.
        false
    }
}

impl AccessPortType for AmbaAxi5 {
    fn ap_address(&self) -> &FullyQualifiedApAddress {
        &self.address
    }
}

impl ApRegAccess<CSW> for AmbaAxi5 {}

super::attached_regs_to_mem_ap!(memory_ap_regs => AmbaAxi5);

define_ap_register!(
    /// Control and Status Word register
    ///
    /// The control and status word register (CSW) is used
    /// to configure memory access through the memory AP.
    name: CSW,
    address: 0xD00,
    fields: [
        /// Is debug software access enabled.
        DbgSwEnable: bool,          // [31]
        Instruction: bool,          // [30]
        /// Is the transaction request non-secure
        ///
        /// - If 1 a non-secure transfer is initiated.
        /// - If 0 and SPIDEN is 1 then a secure transfer is initiated.
        /// - If 0 and SPIDEN is 0 then no transaction is initiated.
        NonSecure: bool,            // [29]
        /// Is this transaction privileged
        Privileged: bool,           // [28]
        /// Drives `AxCACHE[3:0]` where x is R for reads and W for writes.
        /// Amba AXI4 requires asymmetrical usage of `ARCACHE` and `AWCACHE`.
        CACHE: u8,                  // [27:24]
        /// Secure Debug Enabled.
        ///
        /// This bit reflects the state of the CoreSight authentication signal.
        SPIDEN: bool,               // [23]
        /// Memory Tagging Control.
        /// When tagging is enabled, system read and write accesses via DRW, BDx and DARx use T0TR
        /// for transferring tag information.
        MTE: bool,                  // [15]
        /// Drives the AxDOMAIN signals.
        Type: u8,                   // [14:13]
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
        Instruction: ((value >> 30) & 0x01) != 0,
        NonSecure:  ((value >> 29) & 0x01) != 0,
        Privileged: ((value >> 28) & 0x01) != 0,
        CACHE:      ((value >> 24) & 0xF) as u8,
        SPIDEN:     ((value >> 23) & 0x01) != 0,
        MTE:        ((value >> 15) & 0x01) != 0,
        Type:       ((value >> 13) & 0x03) as u8,
        TrInProg:   ((value >> 7) & 0x01) != 0,
        DeviceEn:   ((value >> 6) & 0x01) != 0,
        AddrInc: AddressIncrement::from_u8(((value >> 4) & 0x03) as u8).ok_or_else(|| RegisterParseError::new("CSW", value))?,
        Size: DataSize::try_from((value & 0x07) as u8).map_err(|_| RegisterParseError::new("CSW", value))?,
        _reserved_bits: value & 0x007F_FF08,
    }),
    to: value => (u32::from(value.DbgSwEnable) << 31)
    | (u32::from(value.Instruction  ) << 30)
    | (u32::from(value.NonSecure    ) << 29)
    | (u32::from(value.Privileged   ) << 28)
    | (u32::from(value.CACHE        ) << 24)
    | (u32::from(value.SPIDEN       ) << 23)
    | (u32::from(value.MTE          ) << 15)
    | (u32::from(value.Type         ) << 13)
    | (u32::from(value.TrInProg     ) <<  7)
    | (u32::from(value.DeviceEn     ) <<  6)
    | (u32::from(value.AddrInc as u8) <<  4)
    | (value.Size as u32)
    | value._reserved_bits
);
