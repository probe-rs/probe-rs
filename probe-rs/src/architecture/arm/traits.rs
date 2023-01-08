use crate::{DebugProbe, DebugProbeError};

use super::ArmError;

/// The type of port we are using.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum PortType {
    /// Debug Port (e.g. SWD or JTAG)
    DebugPort,
    /// Access Port (e.g. Memory Access Port)
    AccessPort,
}

bitfield::bitfield! {
    /// A struct to describe the default CMSIS-DAP pins that one can toggle from the host.
    #[derive(Copy, Clone)]
    pub struct Pins(u8);
    impl Debug;
    /// The active low reset of the debug probe.
    pub nreset, set_nreset: 7;
    /// The negative target reset pin of JTAG.
    pub ntrst, set_ntrst: 5;
    /// The TDO or SWO pin.
    pub tdo, set_tdo: 3;
    /// The TDI pin.
    pub tdi, set_tdi: 2;
    /// The SWDIO or TMS pin.
    pub swdio_tms, set_swdio_tms: 1;
    /// The clock pin.
    pub swclk_tck, set_swclk_tck: 0;
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
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct ApAddress {
    /// The address of the debug port this access port belongs to.
    pub dp: DpAddress,
    /// The access port number.
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
    /// Select the debug port to operate on.
    ///
    /// If the probe is connected to a system with multiple debug ports,
    /// this will process all queued commands which haven't been executed yet.
    ///
    /// This means that returned errors can also come from these commands,
    /// not only from changing debug port.
    fn select_dp(&mut self, dp: DpAddress) -> Result<(), ArmError>;

    /// Read a DAP register.
    ///
    /// Only the lowest 4 bits of `addr` are used. Bank switching is the caller's responsibility.
    fn raw_read_register(&mut self, port: PortType, addr: u8) -> Result<u32, ArmError>;

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
    ) -> Result<(), ArmError> {
        for val in values {
            *val = self.raw_read_register(port, addr)?;
        }

        Ok(())
    }

    /// Write a value to a DAP register.
    ///
    /// Only the lowest 4 bits of `addr` are used. Bank switching is the caller's responsibility.
    fn raw_write_register(&mut self, port: PortType, addr: u8, value: u32) -> Result<(), ArmError>;

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
    ) -> Result<(), ArmError> {
        for val in values {
            self.raw_write_register(port, addr, *val)?;
        }

        Ok(())
    }

    /// Flush any outstanding writes.
    ///
    /// By default, this does nothing -- but in probes that implement write
    /// batching, this needs to flush any pending writes.
    fn raw_flush(&mut self) -> Result<(), ArmError> {
        Ok(())
    }

    /// Send a specific output sequence over JTAG or SWD.
    ///
    /// This can only be used for output, and should be used to generate
    /// the initial reset sequence, for example.
    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError>;

    /// Set the state of debugger output pins directly.
    ///
    /// The bits have the following meaning:
    ///
    /// Bit 0: SWCLK/TCK
    /// Bit 1: SWDIO/TMS
    /// Bit 2: TDI
    /// Bit 3: TDO
    /// Bit 5: nTRST
    /// Bit 7: nRESET
    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, DebugProbeError>;

    /// Cast this interface into a generic [`DebugProbe`].
    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe>;
}

/// High-level DAP register access.
///
/// Operations on this trait perform logical register reads/writes. Implementations
/// are responsible for bank switching and AP selection, so one method call can result
/// in multiple transactions on the wire, if necessary.
pub trait DapAccess {
    /// Read a Debug Port register.
    ///
    /// Highest 4 bits of `addr` are interpreted as the bank number, implementations
    /// will do bank switching if necessary.
    ///
    /// If the device uses multiple debug ports, this will switch the active debug port if necessary.
    /// In case this happens, all queued operations will be performed, and returned errors can be from
    /// these operations as well.
    fn read_raw_dp_register(&mut self, dp: DpAddress, addr: u8) -> Result<u32, ArmError>;

    /// Write a Debug Port register.
    ///
    /// Highest 4 bits of `addr` are interpreted as the bank number, implementations
    /// will do bank switching if necessary.
    ///
    /// If the device uses multiple debug ports, this will switch the active debug port if necessary.
    /// In case this happens, all queued operations will be performed, and returned errors can be from
    /// these operations as well.
    fn write_raw_dp_register(
        &mut self,
        dp: DpAddress,
        addr: u8,
        value: u32,
    ) -> Result<(), ArmError>;

    /// Read an Access Port register.
    ///
    /// Highest 4 bits of `addr` are interpreted as the bank number, implementations
    /// will do bank switching if necessary.
    fn read_raw_ap_register(&mut self, ap: ApAddress, addr: u8) -> Result<u32, ArmError>;

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
    ) -> Result<(), ArmError> {
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
    ) -> Result<(), ArmError>;

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
    ) -> Result<(), ArmError> {
        for val in values {
            self.write_raw_ap_register(ap, addr, *val)?;
        }
        Ok(())
    }
}
