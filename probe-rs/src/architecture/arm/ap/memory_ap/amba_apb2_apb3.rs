use crate::architecture::arm::{
    ap::{AccessPortType, ApAccess, ApRegAccess},
    communication_interface::RegisterParseError,
    ArmError, DapAccess, FullyQualifiedApAddress, Register,
};

use super::{registers::AddressIncrement, DataSize};

/// Memory AP
///
/// The memory AP can be used to access a memory-mapped
/// set of debug resources of the attached system.
#[derive(Debug)]
pub struct AmbaApb2Apb3 {
    address: FullyQualifiedApAddress,
    csw: CSW,
    cfg: super::registers::CFG,
}

impl AmbaApb2Apb3 {
    /// Creates a new AmbaAhb3 with `address` as base address.
    pub fn new<P: DapAccess>(
        probe: &mut P,
        address: FullyQualifiedApAddress,
    ) -> Result<Self, ArmError> {
        use crate::architecture::arm::Register;
        let csw = probe.read_raw_ap_register(&address, CSW::ADDRESS)?;
        let cfg = probe.read_raw_ap_register(&address, super::registers::CFG::ADDRESS)?;

        let (csw, cfg) = (csw.try_into()?, cfg.try_into()?);

        let me = Self { address, csw, cfg };
        let csw = CSW {
            AddrInc: AddressIncrement::Single,
            ..me.csw
        };
        probe.write_ap_register(&me, csw)?;
        Ok(Self { csw, ..me })
    }
}

impl super::MemoryApType for AmbaApb2Apb3 {
    type CSW = CSW;

    fn status<P: ApAccess + ?Sized>(&mut self, probe: &mut P) -> Result<CSW, ArmError> {
        #[allow(clippy::assertions_on_constants)]
        const { assert!(super::registers::CSW::ADDRESS == CSW::ADDRESS) };
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
        self.cfg.LA
    }

    fn has_large_data_extension(&self) -> bool {
        self.cfg.LD
    }

    fn supports_only_32bit_data_size(&self) -> bool {
        // APB2 and APB3 AP only support 32bit accesses
        true
    }
}

impl AccessPortType for AmbaApb2Apb3 {
    fn ap_address(&self) -> &FullyQualifiedApAddress {
        &self.address
    }
}

impl ApRegAccess<CSW> for AmbaApb2Apb3 {}

crate::attached_regs_to_mem_ap!(memory_ap_regs => AmbaApb2Apb3);

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
        /// A transfer is in progress.
        /// Can be used to poll whether an aborted transaction has completed.
        /// Read only.
        TrInProg: bool,             // [7]
        /// `1` if transactions can be issued through this access port at the moment.
        /// Read only.
        DeviceEn: bool,             // [6]
        /// Address Auto Increment.
        /// This AP does not support the Packed mode of transfer.
        AddrInc: AddressIncrement,  // [5:4]
        /// The access size of this memory AP.
        /// Only supports word accesses.
        Size: DataSize,             // [2:0]
        /// Reserved bit, kept to preserve IMPLEMENTATION DEFINED statuses.
        _reserved_bits: u32,        // mask
    ],
    from: value => Ok(CSW {
        DbgSwEnable: ((value >> 31) & 0x01) != 0,
        TrInProg: ((value >> 7) & 0x01) != 0,
        DeviceEn: ((value >> 6) & 0x01) != 0,
        AddrInc: AddressIncrement::from_u8(((value >> 4) & 0x03) as u8).ok_or_else(|| RegisterParseError::new("CSW", value))?,
        Size: DataSize::try_from((value & 0x07) as u8).map_err(|_| RegisterParseError::new("CSW", value))?,
        _reserved_bits: (value & 0x7FFF_FF08),
    }),
    to: value => (u32::from(value.DbgSwEnable) << 31)
    | (u32::from(value.TrInProg) << 7)
    | (u32::from(value.DeviceEn) << 6)
    | ((value.AddrInc as u32) << 4)
    | (value.Size as u32)
    | value._reserved_bits
);
