use crate::architecture::arm::{
    ap_v1::{AccessPortType, ApAccess, ApRegAccess, Register},
    ArmError, DapAccess, FullyQualifiedApAddress, RegisterParseError,
};

use super::{registers::AddressIncrement, DataSize};

/// Memory AP
///
/// The memory AP can be used to access a memory-mapped
/// set of debug resources of the attached system.
#[derive(Debug)]
pub struct AmbaApb4Apb5 {
    address: FullyQualifiedApAddress,
    csw: CSW,
    cfg: super::registers::CFG,
}

impl AmbaApb4Apb5 {
    /// Creates a new AmbaAhb3 with `address` as base address.
    pub fn new<P: DapAccess>(
        probe: &mut P,
        address: FullyQualifiedApAddress,
    ) -> Result<Self, ArmError> {
        let csw = probe.read_raw_ap_register(&address, CSW::ADDRESS)?;
        let cfg = probe.read_raw_ap_register(&address, super::registers::CFG::ADDRESS)?;

        let (csw, cfg) = (csw.try_into()?, cfg.try_into()?);

        let me = Self { address, csw, cfg };
        let csw = CSW {
            DbgSwEnable: true,
            AddrInc: AddressIncrement::Single,
            ..me.csw
        };
        probe.write_ap_register(&me, csw)?;
        Ok(Self { csw, ..me })
    }
}

impl super::MemoryApType for AmbaApb4Apb5 {
    type CSW = CSW;

    fn status<P: ApAccess + ?Sized>(&mut self, probe: &mut P) -> Result<CSW, ArmError> {
        #[allow(clippy::assertions_on_constants)]
        const {
            assert!(super::registers::CSW::ADDRESS == CSW::ADDRESS)
        };
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
            _ => Err(ArmError::UnsupportedTransferWidth(data_size as usize * 8)),
        }
    }

    fn has_large_address_extension(&self) -> bool {
        self.cfg.LA
    }

    fn has_large_data_extension(&self) -> bool {
        self.cfg.LD
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

crate::attached_regs_to_mem_ap!(memory_ap_regs => AmbaApb4Apb5);

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
        /// Is a Non-secure transfer requested.
        /// If a secure transfer is requested, the behaviour depends on the value of SPIDEN.
        /// - If SPIDEN is 1 then a secure transfer is initiated.
        /// - IF SPIDEN is 0, then no transfer is initiated. An access to DRW or BD0-BD3 is likely
        ///   to return an error.
        NonSecure: bool,            // [29]
        /// Is this transaction privileged
        Privileged: bool,            // [28]
        /// May reflect the state of the CoreSight authentication interface.
        /// If Secure debug is not supported, this field is always 0.
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
        /// Only supports word accesses.
        Size: DataSize,             // [2:0]
        /// Reserved
        _reserved_bits: u32,        // mask
    ],
    from: value => Ok(CSW {
        DbgSwEnable: ((value >> 31) & 0x01) != 0,
        NonSecure:  ((value >> 29) & 0x01) != 0,
        Privileged: ((value >> 28) & 0x01) != 0,
        SPIDEN:     ((value >> 23) & 0x01) != 0,
        TrInProg:   ((value >> 7) & 0x01) != 0,
        DeviceEn:   ((value >> 6) & 0x01) != 0,
        AddrInc: AddressIncrement::from_u8(((value >> 4) & 0x03) as u8).ok_or_else(|| RegisterParseError::new("CSW", value))?,
        Size: DataSize::try_from((value & 0x07) as u8).map_err(|_| RegisterParseError::new("CSW", value))?,
        _reserved_bits: (value & 0x5F7F_FF08),
    }),
    to: value => (u32::from(value.DbgSwEnable) << 31)
    | (u32::from(value.NonSecure) << 29)
    | (u32::from(value.Privileged) << 28)
    | (u32::from(value.SPIDEN) << 23)
    | (u32::from(value.TrInProg) << 7)
    | (u32::from(value.DeviceEn) << 6)
    | ((value.AddrInc as u32) << 4)
    | (value.Size as u32)
    | value._reserved_bits
);
