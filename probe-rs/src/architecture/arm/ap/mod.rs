//! Types and functions for interacting with access ports.

#[macro_use]
pub mod register_generation;
pub(crate) mod generic_ap;
pub(crate) mod memory_ap;

use crate::architecture::arm::dp::DebugPortError;
use crate::DebugProbeError;

pub use generic_ap::{ApClass, ApType, GenericAp, IDR};
pub use memory_ap::{
    AddressIncrement, BaseaddrFormat, DataSize, MemoryAp, BASE, BASE2, CFG, CSW, DRW, TAR, TAR2,
};

use super::{
    communication_interface::RegisterParseError, ApAddress, ArmError, DapAccess, DpAddress,
    Register,
};

/// Some error during AP handling occurred.
#[derive(Debug, thiserror::Error)]
pub enum AccessPortError {
    /// An error occurred when trying to read a register.
    #[error("Failed to read register {name} at address 0x{address:08x}")]
    RegisterRead {
        /// The address of the register.
        address: u8,
        /// The name if the register.
        name: &'static str,
        /// The underlying root error of this access error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// An error occurred when trying to write a register.
    #[error("Failed to write register {name} at address 0x{address:08x}")]
    RegisterWrite {
        /// The address of the register.
        address: u8,
        /// The name if the register.
        name: &'static str,
        /// The underlying root error of this access error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Some error with the operation of the APs DP occurred.
    #[error("Error while communicating with debug port")]
    DebugPort(#[from] DebugPortError),
    /// An error occurred when trying to flush batched writes of to the AP.
    #[error("Failed to flush batched writes")]
    Flush(#[from] DebugProbeError),

    /// Error while parsing a register
    #[error("Error parsing a register")]
    RegisterParse(#[from] RegisterParseError),
}

impl AccessPortError {
    /// Constructs a [`AccessPortError::RegisterRead`] from just the source error and the register type.
    pub fn register_read_error<R: Register, E: std::error::Error + Send + Sync + 'static>(
        source: E,
    ) -> Self {
        AccessPortError::RegisterRead {
            address: R::ADDRESS,
            name: R::NAME,
            source: Box::new(source),
        }
    }

    /// Constructs a [`AccessPortError::RegisterWrite`] from just the source error and the register type.
    pub fn register_write_error<R: Register, E: std::error::Error + Send + Sync + 'static>(
        source: E,
    ) -> Self {
        AccessPortError::RegisterWrite {
            address: R::ADDRESS,
            name: R::NAME,
            source: Box::new(source),
        }
    }
}

/// A trait to be implemented by access port register types.
///
/// Use the [`define_ap_register!`] macro to implement this.
pub trait ApRegister<PORT: AccessPort>: Register + Sized {}

/// A trait to be implemented on access port types.
///
/// Use the [`define_ap!`] macro to implement this.
pub trait AccessPort {
    /// Returns the address of the access port.
    fn ap_address(&self) -> ApAddress;
}

/// A trait to be implemented by access port drivers to implement access port operations.
pub trait ApAccess {
    /// Read a register of the access port.
    fn read_ap_register<PORT, R>(&mut self, port: impl Into<PORT>) -> Result<R, ArmError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>;

    /// Read a register of the access port using a block transfer.
    /// This can be used to read multiple values from the same register.
    fn read_ap_register_repeated<PORT, R>(
        &mut self,
        port: impl Into<PORT> + Clone,
        register: R,
        values: &mut [u32],
    ) -> Result<(), ArmError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>;

    /// Write a register of the access port.
    fn write_ap_register<PORT, R>(
        &mut self,
        port: impl Into<PORT>,
        register: R,
    ) -> Result<(), ArmError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>;

    /// Write a register of the access port using a block transfer.
    /// This can be used to write multiple values to the same register.
    fn write_ap_register_repeated<PORT, R>(
        &mut self,
        port: impl Into<PORT> + Clone,
        register: R,
        values: &[u32],
    ) -> Result<(), ArmError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>;
}

impl<T: DapAccess> ApAccess for T {
    fn read_ap_register<PORT, R>(&mut self, port: impl Into<PORT>) -> Result<R, ArmError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        tracing::debug!("Reading register {}", R::NAME);
        let raw_value = self.read_raw_ap_register(port.into().ap_address(), R::ADDRESS)?;

        tracing::debug!("Read register    {}, value=0x{:x?}", R::NAME, raw_value);

        Ok(raw_value.try_into()?)
    }

    fn write_ap_register<PORT, R>(
        &mut self,
        port: impl Into<PORT>,
        register: R,
    ) -> Result<(), ArmError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        tracing::debug!("Writing register {}, value={:x?}", R::NAME, register);
        self.write_raw_ap_register(port.into().ap_address(), R::ADDRESS, register.into())
    }

    fn write_ap_register_repeated<PORT, R>(
        &mut self,
        port: impl Into<PORT>,
        _register: R,
        values: &[u32],
    ) -> Result<(), ArmError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        tracing::debug!(
            "Writing register {}, block with len={} words",
            R::NAME,
            values.len(),
        );
        self.write_raw_ap_register_repeated(port.into().ap_address(), R::ADDRESS, values)
    }

    fn read_ap_register_repeated<PORT, R>(
        &mut self,
        port: impl Into<PORT>,
        _register: R,
        values: &mut [u32],
    ) -> Result<(), ArmError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        tracing::debug!(
            "Reading register {}, block with len={} words",
            R::NAME,
            values.len(),
        );

        self.read_raw_ap_register_repeated(port.into().ap_address(), R::ADDRESS, values)
    }
}

/// Determine if an AP exists with the given AP number.
/// Can fail silently under the hood testing an ap that doesnt exist and would require cleanup.
pub fn access_port_is_valid<AP>(debug_port: &mut AP, access_port: GenericAp) -> bool
where
    AP: ApAccess,
{
    let idr_result: Result<IDR, _> = debug_port.read_ap_register(access_port);

    match idr_result {
        Ok(idr) => u32::from(idr) != 0,
        Err(_e) => false,
    }
}

/// Return a Vec of all valid access ports found that the target connected to the debug_probe.
/// Can fail silently under the hood testing an ap that doesnt exist and would require cleanup.
pub(crate) fn valid_access_ports<AP>(debug_port: &mut AP, dp: DpAddress) -> Vec<GenericAp>
where
    AP: ApAccess,
{
    (0..=255)
        .map(|ap| GenericAp::new(ApAddress { dp, ap }))
        .take_while(|port| access_port_is_valid(debug_port, *port))
        .collect::<Vec<GenericAp>>()
}

/// Tries to find the first AP with the given idr value, returns `None` if there isn't any
pub fn get_ap_by_idr<AP, P>(debug_port: &mut AP, dp: DpAddress, f: P) -> Option<GenericAp>
where
    AP: ApAccess,
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
