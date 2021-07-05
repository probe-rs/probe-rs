#[macro_use]
pub mod register_generation;
pub(crate) mod generic_ap;
pub(crate) mod memory_ap;

use crate::architecture::arm::dp::DebugPortError;
use crate::DebugProbeError;

pub use generic_ap::{ApClass, ApType, GenericAp, IDR};
pub use memory_ap::{
    AddressIncrement, BaseaddrFormat, DataSize, MemoryAp, BASE, BASE2, CSW, DRW, TAR,
};
use probe_rs_target::Core;

use super::TypedDapAccess;
use super::{ApAddress, DpAddress, Register};

#[derive(Debug, thiserror::Error)]
pub enum AccessPortError {
    #[error("Failed to access address 0x{address:08x} as it is not aligned to the requirement of {alignment} bytes.")]
    MemoryNotAligned { address: u32, alignment: usize },
    #[error("Failed to read register {name} at address 0x{address:08x}")]
    RegisterReadError {
        address: u8,
        name: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("Failed to write register {name} at address 0x{address:08x}")]
    RegisterWriteError {
        address: u8,
        name: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("Out of bounds access")]
    OutOfBoundsError,
    #[error("Error while communicating with debug port")]
    DebugPort(#[from] DebugPortError),
    #[error("Failed to flush batched writes")]
    FlushError(#[from] DebugProbeError),
    #[error("The target for this core has defined invalid core access options. This is a bug, please file an issue. {0:?}")]
    InvalidCoreAccessOption(Core),
}

impl AccessPortError {
    pub fn register_read_error<R: Register, E: std::error::Error + Send + Sync + 'static>(
        source: E,
    ) -> Self {
        AccessPortError::RegisterReadError {
            address: R::ADDRESS,
            name: R::NAME,
            source: Box::new(source),
        }
    }

    pub fn register_write_error<R: Register, E: std::error::Error + Send + Sync + 'static>(
        source: E,
    ) -> Self {
        AccessPortError::RegisterWriteError {
            address: R::ADDRESS,
            name: R::NAME,
            source: Box::new(source),
        }
    }

    pub fn alignment_error(address: u32, alignment: usize) -> Self {
        AccessPortError::MemoryNotAligned { address, alignment }
    }
}

pub trait ApRegister<PORT: AccessPort>: Register + Sized {}

pub trait AccessPort {
    fn ap_address(&self) -> ApAddress;
}

/// Determine if an AP exists with the given AP number.
/// Can fail silently under the hood testing an ap that doesnt exist and would require cleanup.
pub fn access_port_is_valid<AP>(debug_port: &mut AP, access_port: GenericAp) -> bool
where
    AP: TypedDapAccess,
{
    let idr_result: Result<IDR, DebugProbeError> = debug_port.read_ap_register(access_port);

    match idr_result {
        Ok(idr) => u32::from(idr) != 0,
        Err(_e) => false,
    }
}

/// Return a Vec of all valid access ports found that the target connected to the debug_probe.
/// Can fail silently under the hood testing an ap that doesnt exist and would require cleanup.
pub(crate) fn valid_access_ports<AP>(debug_port: &mut AP, dp: DpAddress) -> Vec<GenericAp>
where
    AP: TypedDapAccess,
{
    (0..=255)
        .map(|ap| GenericAp::new(ApAddress { dp, ap }))
        .take_while(|port| access_port_is_valid(debug_port, *port))
        .collect::<Vec<GenericAp>>()
}

/// Tries to find the first AP with the given idr value, returns `None` if there isn't any
pub fn get_ap_by_idr<AP, P>(debug_port: &mut AP, dp: DpAddress, f: P) -> Option<GenericAp>
where
    AP: TypedDapAccess,
    P: Fn(IDR) -> bool,
{
    (0..=255)
        .map(|ap| GenericAp::new(ApAddress { dp, ap }))
        .find(|ap| {
            if let Ok(idr) = debug_port.read_ap_register(*ap) {
                f(idr)
            } else {
                false
            }
        })
}
