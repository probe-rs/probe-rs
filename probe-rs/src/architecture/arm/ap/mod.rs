//! Types and functions for interacting with access ports.

#[macro_use]
pub mod register_generation;
pub(crate) mod generic_ap;
pub(crate) mod memory_ap;

use crate::architecture::arm::dp::DebugPortError;
use crate::probe::DebugProbeError;

pub use generic_ap::{ApClass, ApType, IDR};

use super::{
    communication_interface::RegisterParseError, ArmError, DapAccess, DpAddress,
    FullyQualifiedApAddress, Register,
};

/// Some error during AP handling occurred.
#[derive(Debug, thiserror::Error)]
pub enum AccessPortError {
    /// An error occurred when trying to read a register.
    #[error("Failed to read register {name} at address {address:#04x}")]
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
    #[error("Failed to write register {name} at address {address:#04x}")]
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

/// A trait to be implemented by ports types providing access to a register.
pub trait ApRegAccess<Reg: Register>: AccessPortType {}

/// A trait to be implemented on access port types.
///
/// Use the [`define_ap!`] macro to implement this.
pub trait AccessPortType {
    /// Returns the address of the access port.
    fn ap_address(&self) -> &FullyQualifiedApAddress;
}

/// A trait to be implemented by access port drivers to implement access port operations.
pub trait ApAccess {
    /// Read a register of the access port.
    fn read_ap_register<PORT, R>(&mut self, port: &PORT) -> Result<R, ArmError>
    where
        PORT: AccessPortType + ApRegAccess<R> + ?Sized,
        R: Register;

    /// Read a register of the access port using a block transfer.
    /// This can be used to read multiple values from the same register.
    fn read_ap_register_repeated<PORT, R>(
        &mut self,
        port: &PORT,
        values: &mut [u32],
    ) -> Result<(), ArmError>
    where
        PORT: AccessPortType + ApRegAccess<R> + ?Sized,
        R: Register;

    /// Write a register of the access port.
    fn write_ap_register<PORT, R>(&mut self, port: &PORT, register: R) -> Result<(), ArmError>
    where
        PORT: AccessPortType + ApRegAccess<R> + ?Sized,
        R: Register;

    /// Write a register of the access port using a block transfer.
    /// This can be used to write multiple values to the same register.
    fn write_ap_register_repeated<PORT, R>(
        &mut self,
        port: &PORT,
        values: &[u32],
    ) -> Result<(), ArmError>
    where
        PORT: AccessPortType + ApRegAccess<R> + ?Sized,
        R: Register;
}

impl<T: DapAccess> ApAccess for T {
    #[tracing::instrument(skip(self, port), fields(ap = port.ap_address().ap_v1().ok(), register = R::NAME, value))]
    fn read_ap_register<PORT, R>(&mut self, port: &PORT) -> Result<R, ArmError>
    where
        PORT: AccessPortType + ApRegAccess<R> + ?Sized,
        R: Register,
    {
        let raw_value = self.read_raw_ap_register(port.ap_address(), R::ADDRESS)?;

        tracing::Span::current().record("value", raw_value);

        tracing::debug!("Register read succesful");

        Ok(raw_value.try_into()?)
    }

    fn write_ap_register<PORT, R>(&mut self, port: &PORT, register: R) -> Result<(), ArmError>
    where
        PORT: AccessPortType + ApRegAccess<R> + ?Sized,
        R: Register,
    {
        tracing::debug!("Writing register {}, value={:x?}", R::NAME, register);
        self.write_raw_ap_register(port.ap_address(), R::ADDRESS, register.into())
    }

    fn write_ap_register_repeated<PORT, R>(
        &mut self,
        port: &PORT,
        values: &[u32],
    ) -> Result<(), ArmError>
    where
        PORT: AccessPortType + ApRegAccess<R> + ?Sized,
        R: Register,
    {
        tracing::debug!(
            "Writing register {}, block with len={} words",
            R::NAME,
            values.len(),
        );
        self.write_raw_ap_register_repeated(port.ap_address(), R::ADDRESS, values)
    }

    fn read_ap_register_repeated<PORT, R>(
        &mut self,
        port: &PORT,
        values: &mut [u32],
    ) -> Result<(), ArmError>
    where
        PORT: AccessPortType + ApRegAccess<R> + ?Sized,
        R: Register,
    {
        tracing::debug!(
            "Reading register {}, block with len={} words",
            R::NAME,
            values.len(),
        );

        self.read_raw_ap_register_repeated(port.ap_address(), R::ADDRESS, values)
    }
}

/// Determine if an AP exists with the given AP number.
///
/// The test is performed by reading the IDR register, and checking if the register is non-zero.
///
/// Can fail silently under the hood testing an ap that doesn't exist and would require cleanup.
pub fn access_port_is_valid<AP>(
    debug_port: &mut AP,
    access_port: &FullyQualifiedApAddress,
) -> Option<IDR>
where
    AP: DapAccess,
{
    let idr_result: Result<IDR, _> = debug_port
        .read_raw_ap_register(access_port, IDR::ADDRESS)
        .and_then(|idr| Ok(IDR::try_from(idr)?));

    match idr_result {
        Ok(idr) if u32::from(idr) != 0 => Some(idr),
        Ok(_) => {
            tracing::debug!("AP {} is not valid, IDR = 0", access_port.ap());
            None
        }
        Err(e) => {
            tracing::debug!(
                "Error reading IDR register from AP {}: {}",
                access_port.ap(),
                e
            );
            None
        }
    }
}

/// Sum-type of the Memory Access Ports.
#[derive(Debug)]
pub enum AccessPort {
    /// Any memory Access Port.
    // TODO: Allow each memory by types to be specialised with there specific feature
    MemoryAp(memory_ap::MemoryAp),
    /// Other Access Ports not used for memory accesses.
    Other(GenericAp),
}
impl AccessPortType for AccessPort {
    fn ap_address(&self) -> &FullyQualifiedApAddress {
        match self {
            AccessPort::MemoryAp(mem_ap) => mem_ap.ap_address(),
            AccessPort::Other(o) => o.ap_address(),
        }
    }
}

/// Return a Vec of all valid access ports found that the target connected to the debug_probe.
/// Can fail silently under the hood testing an ap that doesn't exist and would require cleanup.
#[tracing::instrument(skip(debug_port))]
pub(crate) fn valid_access_ports<DP>(
    debug_port: &mut DP,
    dp: DpAddress,
) -> Vec<FullyQualifiedApAddress>
where
    DP: DapAccess,
{
    valid_access_ports_allowlist(debug_port, dp, 0..=255)
}

/// Return a Vec of all valid access ports found that the target connected to the debug_probe.
/// The search is limited to `allowed_aps`.
///
/// Can fail silently under the hood testing an ap that doesn't exist and would require cleanup.
#[tracing::instrument(skip(debug_port, allowed_aps))]
pub(crate) fn valid_access_ports_allowlist<DP>(
    debug_port: &mut DP,
    dp: DpAddress,
    allowed_aps: impl IntoIterator<Item = u8>,
) -> Vec<FullyQualifiedApAddress>
where
    DP: DapAccess,
{
    allowed_aps
        .into_iter()
        .map_while(|ap| {
            let ap = FullyQualifiedApAddress::v1_with_dp(dp, ap);
            access_port_is_valid(debug_port, &ap).map(|_| ap)
        })
        .collect()
}

/// Tries to find the first AP with the given idr value, returns `None` if there isn't any
pub fn get_ap_by_idr<AP, P>(debug_port: &mut AP, dp: DpAddress, f: P) -> Option<GenericAp>
where
    AP: ApAccess,
    P: Fn(IDR) -> bool,
{
    (0..=255)
        .map(|ap| GenericAp::new(FullyQualifiedApAddress::v1_with_dp(dp, ap)))
        .find(|ap| {
            if let Ok(idr) = debug_port.read_ap_register(ap) {
                f(idr)
            } else {
                false
            }
        })
}

define_ap!(
    /// A generic access port which implements just the register every access port has to implement
    /// to be compliant with the ADI 5.2 specification.
    GenericAp
);
impl ApRegAccess<IDR> for GenericAp {}
