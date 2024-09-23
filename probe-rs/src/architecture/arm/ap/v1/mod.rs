//! Types and functions for interacting with the first version of access ports.
use crate::architecture::arm::dp::DebugPortError;
use crate::probe::DebugProbeError;

use super::{memory::{registers::{self, BaseAddrFormat, BASE, BASE2, DRW, TAR, TAR2}, DataSize, MemoryAp}, GenericAp};
use super::{
    super::{dp::DpAddress, ArmError, DapAccess, FullyQualifiedApAddress, RegisterParseError},
    IDR,
};

/// A trait to be implemented on Access Port register types for typed device access.
pub trait Register:
    Clone + TryFrom<u32, Error = RegisterParseError> + Into<u32> + Sized + std::fmt::Debug
{
    /// The address of the register (in bytes).
    const ADDRESS: u8;
    /// The name of the register as string.
    const NAME: &'static str;
}

pub(crate) trait MemoryApType:
    ApRegAccess<BASE> + ApRegAccess<BASE2> + ApRegAccess<TAR> + ApRegAccess<TAR2> + ApRegAccess<DRW>
{
    /// This Memory AP’s specific CSW type.
    type CSW: Register;

    fn has_large_address_extension(&self) -> bool;
    fn has_large_data_extension(&self) -> bool;
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
    fn generic_status<I: ApAccess>(
        &mut self,
        interface: &mut I,
    ) -> Result<registers::CSW, ArmError> {
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

        let mut base_address = if BaseAddrFormat::ADIv5 == base_register.Format {
            let base2: BASE2 = interface.read_ap_register(self)?;

            u64::from(base2.BASEADDR) << 32
        } else {
            0
        };
        base_address |= u64::from(base_register.BASEADDR << 12);

        Ok(base_address)
    }

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
            tracing::debug!("AP {:?} is not valid, IDR = 0", access_port.ap());
            None
        }
        Err(e) => {
            tracing::debug!(
                "Error reading IDR register from AP {:?}: {}",
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
    MemoryAp(MemoryAp),
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
