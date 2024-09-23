//! Memory access port

pub(crate) mod mock;
pub mod registers;

mod amba_ahb3;
mod amba_apb2_apb3;
mod amba_apb4_apb5;

mod amba_ahb5;
mod amba_ahb5_hprot;

mod amba_axi3_axi4;
mod amba_axi5;

pub use registers::DataSize;
use registers::{AddressIncrement, DRW, TAR};

use super::v1::{AccessPortType, ApAccess, ApRegAccess};
use crate::architecture::arm::{ArmError, DapAccess, FullyQualifiedApAddress};

use super::v1::MemoryApType;

/// Implements all default registers of a memory AP to the given type.
///
/// Invoke in the form `attached_regs_to_mem_ap!(mod_name => ApName)` where:
/// - `mod_name` is a module name in which the impl an the required use will be expanded to.
/// - `ApName` a type name that must be available in the current scope to which the registers will
///   be attached.
#[macro_export]
macro_rules! attached_regs_to_mem_ap {
    ($mod_name:ident => $name:ident) => {
        mod $mod_name {
            use super::$name;
            use $crate::architecture::arm::ap::{
                memory::registers::{
                    BASE, BASE2, BD0, BD1, BD2, BD3, CFG, CSW, DRW, MBT, TAR, TAR2,
                },
                v1::ApRegAccess,
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

macro_rules! memory_aps {
    ($($variant:ident => $type:path),*) => {
        #[derive(Debug)]
        pub enum MemoryAp {
            $($variant($type)),*
        }

        $(impl From<$type> for MemoryAp {
            fn from(value: $type) -> Self {
                Self::$variant(value)
            }
        })*

        impl MemoryAp {
            pub fn new<I: DapAccess>(
                interface: &mut I,
                address: &FullyQualifiedApAddress,
            ) -> Result<Self, ArmError> {
                use $crate::architecture::arm::ap::{IDR, v1::Register};
                let idr: IDR = interface
                    .read_raw_ap_register(address, IDR::ADDRESS)?
                    .try_into()?;
                tracing::debug!("reading IDR: {:x?}", idr);
                use $crate::architecture::arm::ap::ApType;
                Ok(match idr.TYPE {
                    ApType::JtagComAp => return Err(ArmError::WrongApType),
                    $(ApType::$variant => <$type>::new(interface, address.clone())?.into(),)*
                })
            }
        }
    }
}

memory_aps! {
    AmbaAhb3 => amba_ahb3::AmbaAhb3,
    AmbaAhb5 => amba_ahb5::AmbaAhb5,
    AmbaAhb5Hprot => amba_ahb5_hprot::AmbaAhb5Hprot,
    AmbaApb2Apb3 => amba_apb2_apb3::AmbaApb2Apb3,
    AmbaApb4Apb5 => amba_apb4_apb5::AmbaApb4Apb5,
    AmbaAxi3Axi4 => amba_axi3_axi4::AmbaAxi3Axi4,
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

impl super::v1::MemoryApType for MemoryAp {
    type CSW = registers::CSW;

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
