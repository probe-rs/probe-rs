use crate::DebugProbeError;

use super::{
    ap::{AccessPort, ApRegister},
    dp::{DebugPortError, DpRegister},
};

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum PortType {
    DebugPort,
    AccessPort,
}

/// Debug port address.
#[derive(Debug, Eq, PartialEq, Clone, Copy, Hash)]
pub enum DpAddress {
    /// Access the single DP on the bus, assuming there is only one.
    /// Will cause corruption if multiple are present.
    Default,
    /// Select a particular DP on a SWDv2 multidrop bus. The contained `u32` is
    /// the `TARGETSEL` value to select it.
    Multidrop(u32),
}

/// Access port address.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct ApAddress {
    pub dp: DpAddress,
    pub ap: u8,
}

/// Low-level DAP register access.
///
/// Operations on this trait closely match the transactions on the wire. Implementors
/// only do basic error handling, such as retrying WAIT errors.
///
/// Almost everything is the responsibility of the caller. For example, the caller must
/// handle bank switching and AP selection.
pub trait RawDapAccess {
    fn select_dp(&mut self, dp: DpAddress) -> Result<(), DebugProbeError>;

    /// Read a DAP register.
    ///
    /// Only the lowest 4 bits of `addr` are used. Bank switching is the caller's responsibility.
    fn raw_read_register(&mut self, port: PortType, addr: u8) -> Result<u32, DebugProbeError>;

    /// Read multiple values from the same DAP register.
    ///
    /// If possible, this uses optimized read functions, otherwise it
    /// falls back to the `read_register` function.
    ///
    /// Only the lowest 4 bits of `addr` are used. Bank switching is the caller's responsibility.
    fn raw_read_block(
        &mut self,
        port: PortType,
        addr: u8,
        values: &mut [u32],
    ) -> Result<(), DebugProbeError> {
        for val in values {
            *val = self.raw_read_register(port, addr)?;
        }

        Ok(())
    }

    /// Write a value to a DAP register.
    ///
    /// Only the lowest 4 bits of `addr` are used. Bank switching is the caller's responsibility.
    fn raw_write_register(
        &mut self,
        port: PortType,
        addr: u8,
        value: u32,
    ) -> Result<(), DebugProbeError>;

    /// Write multiple values to the same DAP register.
    ///
    /// If possible, this uses optimized write functions, otherwise it
    /// falls back to the `write_register` function.
    ///
    /// Only the lowest 4 bits of `addr` are used. Bank switching is the caller's responsibility.
    fn raw_write_block(
        &mut self,
        port: PortType,
        addr: u8,
        values: &[u32],
    ) -> Result<(), DebugProbeError> {
        for val in values {
            self.raw_write_register(port, addr, *val)?;
        }

        Ok(())
    }

    /// Flush any outstanding writes.
    ///
    /// By default, this does nothing -- but in probes that implement write
    /// batching, this needs to flush any pending writes.
    fn raw_flush(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }
}

/// High-level DAP register access.
///
/// Operations on this trait perform logical register reads/writes. Implementations
/// are responsible for bank switching and AP selection, so one method call can result
/// in multiple transactions on the wire, if necessary.
pub trait UntypedDapAccess {
    /// Read a Debug Port register.
    ///
    /// Highest 4 bits of `addr` are interpreted as the bank number, implementations
    /// will do bank switching if necessary.
    fn read_raw_dp_register(&mut self, dp: DpAddress, addr: u8) -> Result<u32, DebugProbeError>;

    /// Write a Debug Port register.
    ///
    /// Highest 4 bits of `addr` are interpreted as the bank number, implementations
    /// will do bank switching if necessary.
    fn write_raw_dp_register(
        &mut self,
        dp: DpAddress,
        addr: u8,
        value: u32,
    ) -> Result<(), DebugProbeError>;

    /// Read an Access Port register.
    ///
    /// Highest 4 bits of `addr` are interpreted as the bank number, implementations
    /// will do bank switching if necessary.
    fn read_raw_ap_register(&mut self, ap: ApAddress, addr: u8) -> Result<u32, DebugProbeError>;

    /// Read multiple values from the same Access Port register.
    ///
    /// If possible, this uses optimized read functions, otherwise it
    /// falls back to the `read_raw_ap_register` function.
    ///
    /// Highest 4 bits of `addr` are interpreted as the bank number, implementations
    /// will do bank switching if necessary.
    fn read_raw_ap_register_repeated(
        &mut self,
        ap: ApAddress,
        addr: u8,
        values: &mut [u32],
    ) -> Result<(), DebugProbeError> {
        for val in values {
            *val = self.read_raw_ap_register(ap, addr)?;
        }
        Ok(())
    }

    /// Write an AP register.
    ///
    /// Highest 4 bits of `addr` are interpreted as the bank number, implementations
    /// will do bank switching if necessary.
    fn write_raw_ap_register(
        &mut self,
        ap: ApAddress,
        addr: u8,
        value: u32,
    ) -> Result<(), DebugProbeError>;

    /// Write multiple values to the same Access Port register.
    ///
    /// If possible, this uses optimized write functions, otherwise it
    /// falls back to the `write_raw_ap_register` function.
    ///
    /// Highest 4 bits of `addr` are interpreted as the bank number, implementations
    /// will do bank switching if necessary.
    fn write_raw_ap_register_repeated(
        &mut self,
        ap: ApAddress,
        addr: u8,
        values: &[u32],
    ) -> Result<(), DebugProbeError> {
        for val in values {
            self.write_raw_ap_register(ap, addr, *val)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DebugProbeError>;
}

pub trait TypedDapAccess {
    fn read_ap_register<PORT, R>(&mut self, port: impl Into<PORT>) -> Result<R, DebugProbeError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>;

    /// Read a register using a block transfer. This can be used
    /// to read multiple values from the same register.
    fn read_ap_register_repeated<PORT, R>(
        &mut self,
        port: impl Into<PORT> + Clone,
        register: R,
        values: &mut [u32],
    ) -> Result<(), DebugProbeError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>;

    fn write_ap_register<PORT, R>(
        &mut self,
        port: impl Into<PORT>,
        register: R,
    ) -> Result<(), DebugProbeError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>;

    /// Write a register using a block transfer. This can be used
    /// to write multiple values to the same register.
    fn write_ap_register_repeated<PORT, R>(
        &mut self,
        port: impl Into<PORT> + Clone,
        register: R,
        values: &[u32],
    ) -> Result<(), DebugProbeError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>;

    fn read_dp_register<R: DpRegister>(&mut self, dp: DpAddress) -> Result<R, DebugPortError>;

    fn write_dp_register<R: DpRegister>(
        &mut self,
        dp: DpAddress,
        register: R,
    ) -> Result<(), DebugPortError>;

    fn flush(&mut self) -> Result<(), DebugProbeError>;
}

impl<T: UntypedDapAccess> TypedDapAccess for T {
    fn read_ap_register<PORT, R>(&mut self, port: impl Into<PORT>) -> Result<R, DebugProbeError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        log::debug!("Reading register {}", R::NAME);
        let raw_value = self.read_raw_ap_register(port.into().ap_address(), R::ADDRESS)?;

        log::debug!("Read register    {}, value=0x{:x?}", R::NAME, raw_value);

        Ok(raw_value.into())
    }

    fn write_ap_register<PORT, R>(
        &mut self,
        port: impl Into<PORT>,
        register: R,
    ) -> Result<(), DebugProbeError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        log::debug!("Writing register {}, value={:x?}", R::NAME, register);
        self.write_raw_ap_register(port.into().ap_address(), R::ADDRESS, register.into())
    }

    fn write_ap_register_repeated<PORT, R>(
        &mut self,
        port: impl Into<PORT>,
        _register: R,
        values: &[u32],
    ) -> Result<(), DebugProbeError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        log::debug!(
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
    ) -> Result<(), DebugProbeError>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        log::debug!(
            "Reading register {}, block with len={} words",
            R::NAME,
            values.len(),
        );

        self.read_raw_ap_register_repeated(port.into().ap_address(), R::ADDRESS, values)
    }

    fn read_dp_register<R: DpRegister>(&mut self, dp: DpAddress) -> Result<R, DebugPortError> {
        log::debug!("Reading DP register {}", R::NAME);
        let result = self.read_raw_dp_register(dp, R::ADDRESS)?;
        log::debug!("Read    DP register {}, value=0x{:08x}", R::NAME, result);
        Ok(result.into())
    }

    fn write_dp_register<R: DpRegister>(
        &mut self,
        dp: DpAddress,
        register: R,
    ) -> Result<(), DebugPortError> {
        let value = register.into();
        log::debug!("Writing DP register {}, value=0x{:08x}", R::NAME, value);
        self.write_raw_dp_register(dp, R::ADDRESS, value)?;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DebugProbeError> {
        UntypedDapAccess::flush(self)
    }
}
