use crate::DebugProbeError;

use super::dp::DebugPortVersion;

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum PortType {
    DebugPort,
    AccessPort,
}

/// Low-level DAP register access.
///
/// Operations on this trait closely match the transactions on the wire. Implementors
/// only do basic error handling, such as retrying WAIT errors.
///
/// Almost everything is the responsibility of the caller. For example, the caller must
/// handle bank switching and AP selection.
pub trait RawDapAccess {
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
pub trait DapAccess {
    /// Version of the Debug Port implementation.
    fn debug_port_version(&self) -> DebugPortVersion;

    /// Read a Debug Port register.
    ///
    /// Highest 4 bits of `addr` are interpreted as the bank number, implementations
    /// will do bank switching if necessary.
    fn read_raw_dp_register(&mut self, addr: u8) -> Result<u32, DebugProbeError>;

    /// Write a Debug Port register.
    ///
    /// Highest 4 bits of `addr` are interpreted as the bank number, implementations
    /// will do bank switching if necessary.
    fn write_raw_dp_register(&mut self, addr: u8, value: u32) -> Result<(), DebugProbeError>;

    /// Read an Access Port register.
    ///
    /// Highest 4 bits of `addr` are interpreted as the bank number, implementations
    /// will do bank switching if necessary.
    fn read_raw_ap_register(&mut self, port: u8, addr: u8) -> Result<u32, DebugProbeError>;

    /// Read multiple values from the same Access Port register.
    ///
    /// If possible, this uses optimized read functions, otherwise it
    /// falls back to the `read_raw_ap_register` function.
    ///
    /// Highest 4 bits of `addr` are interpreted as the bank number, implementations
    /// will do bank switching if necessary.
    fn read_raw_ap_register_repeated(
        &mut self,
        port: u8,
        addr: u8,
        values: &mut [u32],
    ) -> Result<(), DebugProbeError> {
        for val in values {
            *val = self.read_raw_ap_register(port, addr)?;
        }
        Ok(())
    }

    /// Write an AP register.
    ///
    /// Highest 4 bits of `addr` are interpreted as the bank number, implementations
    /// will do bank switching if necessary.
    fn write_raw_ap_register(
        &mut self,
        port: u8,
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
        port: u8,
        addr: u8,
        values: &[u32],
    ) -> Result<(), DebugProbeError> {
        for val in values {
            self.write_raw_ap_register(port, addr, *val)?;
        }
        Ok(())
    }
}
