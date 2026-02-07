//! Memory access port

pub(crate) mod mock;

mod amba_ahb3;
mod amba_apb2_apb3;
mod amba_apb4_apb5;

mod amba_ahb5;
mod amba_ahb5_hprot;

mod amba_axi3_axi4;
mod amba_axi5;

use crate::architecture::arm::ap::{
    AccessPortError, AddressIncrement, ApRegister, BASE, BASE2, BaseAddrFormat, DRW, DataSize, TAR,
    TAR2,
};

use super::{AccessPortType, ApAccess, ApRegAccess};
use crate::architecture::arm::{ArmError, DapAccess, FullyQualifiedApAddress, ap::CSW};

/// Implements all default registers of a memory AP to the given type.
///
/// Invoke in the form `attached_regs_to_mem_ap!(mod_name => ApName)` where:
/// - `mod_name` is a module name in which the impl an the required use will be expanded to.
/// - `ApName` a type name that must be available in the current scope to which the registers will
///   be attached.
macro_rules! attached_regs_to_mem_ap {
    ($mod_name:ident => $name:ident) => {
        mod $mod_name {
            use super::$name;
            use $crate::architecture::arm::ap::{
                ApRegAccess, BASE, BASE2, BD0, BD1, BD2, BD3, CFG, CSW, DRW, MBT, TAR, TAR2,
            };
            impl ApRegAccess<CFG> for $name {}
            impl ApRegAccess<CSW> for $name {}
            impl ApRegAccess<BASE> for $name {}
            impl ApRegAccess<BASE2> for $name {}
            impl ApRegAccess<TAR> for $name {}
            impl ApRegAccess<TAR2> for $name {}
            impl ApRegAccess<BD2> for $name {}
            impl ApRegAccess<BD3> for $name {}
            impl ApRegAccess<DRW> for $name {}
            impl ApRegAccess<MBT> for $name {}
            impl ApRegAccess<BD1> for $name {}
            impl ApRegAccess<BD0> for $name {}
        }
    };
}

// Re-export the macro so that it can be used in this crate.
pub(crate) use attached_regs_to_mem_ap;

/// Common trait for all memory access ports.
pub trait MemoryApType:
    ApRegAccess<BASE> + ApRegAccess<BASE2> + ApRegAccess<TAR> + ApRegAccess<TAR2> + ApRegAccess<DRW>
{
    /// This Memory AP’s specific CSW type.
    type CSW: ApRegister;

    /// Returns whether the Memory AP supports the large address extension.
    ///
    /// With the large address extension, the address is 64 bits wide.
    fn has_large_address_extension(&self) -> bool;

    /// Returns whether the Memory AP supports the large data extension.
    ///
    /// With the large data extension, the data size can be up to 64 bits wide.
    fn has_large_data_extension(&self) -> bool;

    /// Returns whether the Memory AP only supports 32 bit data size.
    fn supports_only_32bit_data_size(&self) -> bool;

    /// Attempts to set the requested data size.
    ///
    /// The operation may fail if the requested data size is not supported by the Memory Access
    /// Port.
    fn try_set_datasize<I: ApAccess>(
        &mut self,
        interface: &mut I,
        data_size: DataSize,
    ) -> Result<(), ArmError>;

    /// The current generic CSW (missing the memory AP specific fields).
    fn generic_status<I: ApAccess>(&mut self, interface: &mut I) -> Result<CSW, ArmError> {
        self.status(interface)?
            .into()
            .try_into()
            .map_err(ArmError::RegisterParse)
    }

    /// The current CSW with the memory AP specific fields.
    fn status<I: ApAccess>(&mut self, interface: &mut I) -> Result<Self::CSW, ArmError>;

    /// The base address of this AP which is used to then access all relative control registers.
    fn base_address<I: ApAccess>(&self, interface: &mut I) -> Result<u64, ArmError> {
        let base_register: BASE = interface.read_ap_register(self)?;
        if !base_register.present {
            return Err(ArmError::Other("debug entry not present".to_string()));
        }

        let mut base_address = if BaseAddrFormat::ADIv5 == base_register.Format {
            let base2: BASE2 = interface.read_ap_register(self)?;

            u64::from(base2.BASEADDR) << 32
        } else {
            0
        };
        base_address |= u64::from(base_register.BASEADDR << 12);

        Ok(base_address)
    }

    /// Set the target address for the next access.
    ///
    /// This writes the TAR register, and optionally TAR2 register if the address is larger than 32 bits,
    /// and the large address extension is supported.
    fn set_target_address<I: ApAccess>(
        &mut self,
        interface: &mut I,
        address: u64,
    ) -> Result<(), ArmError> {
        let address_lower = address as u32;
        let address_upper = (address >> 32) as u32;

        if self.has_large_address_extension() {
            let tar = TAR2 {
                address: address_upper,
            };
            interface.write_ap_register(self, tar)?;
        } else if address_upper != 0 {
            return Err(ArmError::OutOfBounds);
        }

        let tar = TAR {
            address: address_lower,
        };
        interface.write_ap_register(self, tar)?;

        Ok(())
    }

    /// Read multiple 32 bit values from the DRW register on the given AP.
    fn read_data<I: ApAccess>(
        &mut self,
        interface: &mut I,
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        match values {
            // If transferring only 1 word, use non-repeated register access, because it might be
            // faster depending on the probe.
            [value] => interface.read_ap_register(self).map(|drw: DRW| {
                *value = drw.data;
            }),
            _ => interface.read_ap_register_repeated::<_, DRW>(self, values),
        }
        .map_err(AccessPortError::register_read_error::<DRW, _>)
        .map_err(|err| ArmError::from_access_port(err, self.ap_address()))
    }

    /// Write multiple 32 bit values to the DRW register on the given AP.
    fn write_data<I: ApAccess>(
        &mut self,
        interface: &mut I,
        values: &[u32],
    ) -> Result<(), ArmError> {
        match values {
            // If transferring only 1 word, use non-repeated register access, because it might be
            // faster depending on the probe.
            &[data] => interface.write_ap_register(self, DRW { data }),
            _ => interface.write_ap_register_repeated::<_, DRW>(self, values),
        }
        .map_err(AccessPortError::register_write_error::<DRW, _>)
        .map_err(|e| ArmError::from_access_port(e, self.ap_address()))
    }
}

macro_rules! memory_aps {
    (
        $(
            $(#[$outer:meta])*
            $variant:ident => $type:path
        ),*
    ) => {
        /// Sum type for all memory access ports.
        #[derive(Debug)]
        pub enum MemoryAp {
            $(
                $(#[$outer])*
                $variant($type)
            ),*
        }

        $(impl From<$type> for MemoryAp {
            fn from(value: $type) -> Self {
                Self::$variant(value)
            }
        })*

        impl MemoryAp {
            pub(crate) fn new<I: DapAccess>(
                interface: &mut I,
                address: &FullyQualifiedApAddress,
            ) -> Result<Self, ArmError> {
                use $crate::architecture::arm::ap::{IDR, ApRegister};
                let idr_raw = interface.read_raw_ap_register(address, IDR::ADDRESS)?;
                if idr_raw == 0 {
                    return Err(ArmError::InvalidIdrValue);
                }
                let idr: IDR = idr_raw.try_into()?;
                tracing::debug!("reading IDR: {:x?}", idr);
                use crate::architecture::arm::ap::ApType;
                Ok(match idr.TYPE {
                    ApType::JtagComAp => return Err(ArmError::WrongApType),
                    $(ApType::$variant => <$type>::new(interface, address.clone())?.into(),)*
                })
            }
        }
    }
}

memory_aps! {
    /// AHB3 memory access port.
    AmbaAhb3 => amba_ahb3::AmbaAhb3,
    /// AHB5 memory access port.
    AmbaAhb5 => amba_ahb5::AmbaAhb5,
    /// AHB5 memory access port with enhanced HPROT control.
    AmbaAhb5Hprot => amba_ahb5_hprot::AmbaAhb5Hprot,
    /// APB2 or APB3 memory access port.
    AmbaApb2Apb3 => amba_apb2_apb3::AmbaApb2Apb3,
    /// APB4 or APB5 memory access port.
    AmbaApb4Apb5 => amba_apb4_apb5::AmbaApb4Apb5,
    /// AXI3 or AXI4 memory access port.
    AmbaAxi3Axi4 => amba_axi3_axi4::AmbaAxi3Axi4,
    /// AXI5 memory access port
    AmbaAxi5 => amba_axi5::AmbaAxi5
}

impl ApRegAccess<super::IDR> for MemoryAp {}
attached_regs_to_mem_ap!(memory_ap_regs => MemoryAp);

macro_rules! mem_ap_forward {
    ($me:ident, $name:ident($($arg:ident),*)) => {
        match $me {
            MemoryAp::AmbaApb2Apb3(ap) => ap.$name($($arg),*),
            MemoryAp::AmbaApb4Apb5(ap) => ap.$name($($arg),*),
            MemoryAp::AmbaAhb3(m) => m.$name($($arg),*),
            MemoryAp::AmbaAhb5(m) => m.$name($($arg),*),
            MemoryAp::AmbaAhb5Hprot(m) => m.$name($($arg),*),
            MemoryAp::AmbaAxi3Axi4(m) => m.$name($($arg),*),
            MemoryAp::AmbaAxi5(m) => m.$name($($arg),*),
        }
    }
}
impl AccessPortType for MemoryAp {
    fn ap_address(&self) -> &crate::architecture::arm::FullyQualifiedApAddress {
        mem_ap_forward!(self, ap_address())
    }
}

impl MemoryApType for MemoryAp {
    type CSW = CSW;

    fn has_large_address_extension(&self) -> bool {
        mem_ap_forward!(self, has_large_address_extension())
    }

    fn has_large_data_extension(&self) -> bool {
        mem_ap_forward!(self, has_large_data_extension())
    }

    fn supports_only_32bit_data_size(&self) -> bool {
        mem_ap_forward!(self, supports_only_32bit_data_size())
    }

    fn try_set_datasize<I: ApAccess>(
        &mut self,
        interface: &mut I,
        data_size: DataSize,
    ) -> Result<(), ArmError> {
        mem_ap_forward!(self, try_set_datasize(interface, data_size))
    }

    fn status<I: ApAccess>(&mut self, interface: &mut I) -> Result<Self::CSW, ArmError> {
        mem_ap_forward!(self, generic_status(interface))
    }
}
