use crate::DebugProbeError;

use super::dp::{DebugPortError, DebugPortVersion};

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum PortType {
    DebugPort,
    AccessPort,
}

pub trait RawDapAccess {
    /// Reads the DAP register on the specified port and address
    fn raw_read_register(&mut self, port: PortType, addr: u8) -> Result<u32, DebugProbeError>;

    /// Read multiple values from the same DAP register.
    ///
    /// If possible, this uses optimized read functions, otherwise it
    /// falls back to the `read_register` function.
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

    /// Writes a value to the DAP register on the specified port and address
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

/// An interface to write arbitrary Debug Port registers freely in a type-unsafe manner identifying them by bank number and register address.
/// For a type-safe interface see the [DpAccess] trait.
pub trait RawDpAccess {
    /// Reads a Debug Port register on the Chip.
    fn read_raw_dp_register(&mut self, address: u8) -> Result<u32, DebugPortError>;

    /// Writes a Debug Port register on the Chip.
    fn write_raw_dp_register(&mut self, address: u8, value: u32) -> Result<(), DebugPortError>;

    /// Returns the version of the Debug Port implementation.
    fn debug_port_version(&self) -> DebugPortVersion;
}

/// Direct access to AP registers.
pub trait RawApAccess {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Read a AP register.
    fn read_raw_ap_register(&mut self, port_number: u8, address: u8) -> Result<u32, Self::Error>;

    /// Read a register using a block transfer. This can be used
    /// to read multiple values from the same register.
    fn read_raw_ap_register_repeated(
        &mut self,
        port: u8,
        address: u8,
        values: &mut [u32],
    ) -> Result<(), Self::Error> {
        for val in values {
            *val = self.read_raw_ap_register(port, address)?;
        }
        Ok(())
    }

    /// Write a AP register.
    fn write_raw_ap_register(
        &mut self,
        port: u8,
        address: u8,
        value: u32,
    ) -> Result<(), Self::Error>;

    /// Write a register using a block transfer. This can be used
    /// to write multiple values to the same register.
    fn write_raw_ap_register_repeated(
        &mut self,
        port: u8,
        address: u8,
        values: &[u32],
    ) -> Result<(), Self::Error> {
        for val in values {
            self.write_raw_ap_register(port, address, *val)?;
        }
        Ok(())
    }
}
