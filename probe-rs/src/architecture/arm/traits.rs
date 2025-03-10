use crate::{
    CoreStatus,
    probe::{DebugProbe, DebugProbeError},
};

use super::{
    ArmError,
    communication_interface::DapProbe,
    dp::{DpAddress, DpRegisterAddress},
};

/// Specifies the address of register to access in a debug or access port.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum RegisterAddress {
    /// A Debug Port Register address.
    DpRegister(DpRegisterAddress),
    /// The lowest significant byte of an Access Port Register address.
    ApRegister(u8),
}

const A2_MASK: u8 = 0b0100;
const A3_MASK: u8 = 0b1000;
const A2AND3_MASK: u8 = A2_MASK | A3_MASK;
impl RegisterAddress {
    /// Is this Port Address for an Access Port?
    pub fn is_ap(&self) -> bool {
        !matches!(self, RegisterAddress::DpRegister(_))
    }

    /// The least significant byte of the address.
    pub fn lsb(&self) -> u8 {
        match self {
            RegisterAddress::DpRegister(r) => r.address,
            RegisterAddress::ApRegister(r) => *r,
        }
    }

    /// returns bits 2-3 of the address
    pub fn a2_and_3(&self) -> u8 {
        self.lsb() & A2AND3_MASK
    }

    /// Returns the second bit of the address
    pub fn a2(&self) -> bool {
        (self.lsb() & A2_MASK) == A2_MASK
    }

    /// Returns the third bit of the address
    pub fn a3(&self) -> bool {
        (self.lsb() & A3_MASK) == A3_MASK
    }
}
impl From<DpRegisterAddress> for RegisterAddress {
    fn from(value: DpRegisterAddress) -> Self {
        RegisterAddress::DpRegister(value)
    }
}

impl From<ApAddress> for RegisterAddress {
    fn from(value: ApAddress) -> Self {
        match value {
            ApAddress::V1(addr) => RegisterAddress::ApRegister(addr),
            ApAddress::V2(addr) => match addr.0 {
                Some(addr) => RegisterAddress::ApRegister(addr as u8),
                None => panic!("Something unexpected happened. This is a bug, please report it."),
            },
        }
    }
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

/// Access port v2 address, the base of the AP within the root memory space.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Hash)]
pub struct ApV2Address(pub Option<u64>);

impl ApV2Address {
    /// An AP address for the root component of the root memory interface.
    pub fn root() -> Self {
        Self(None)
    }

    /// Create a new ApV2 address at `base` within the DP root memory space.
    pub fn new(base: u64) -> Self {
        Self(Some(base))
    }
}

/// Access port address
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Hash)]
pub enum ApAddress {
    /// Access port v1 address
    V1(u8),
    /// Access Port v2
    V2(ApV2Address),
}

impl ApAddress {
    /// Check if an AP address is an APv2 address.
    pub fn is_v2(&self) -> bool {
        matches!(self, ApAddress::V2(_))
    }
}

impl std::fmt::Display for ApV2Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl std::fmt::Display for ApAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApAddress::V1(v) => write!(f, "V1({})", v),
            ApAddress::V2(v) => write!(f, "V2({})", v),
        }
    }
}

/// Access port address.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Hash)]
pub struct FullyQualifiedApAddress {
    /// The address of the debug port this access port belongs to.
    dp: DpAddress,
    /// The access port number.
    ap: ApAddress,
}

impl FullyQualifiedApAddress {
    /// Create a new `FullyQualifiedApAddress` belonging to the default debug port.
    pub const fn v1_with_default_dp(ap: u8) -> Self {
        Self {
            dp: DpAddress::Default,
            ap: ApAddress::V1(ap),
        }
    }

    /// Create a new `FullyQualifiedApAddress` belonging to the given debug port using Ap Address
    /// in the version 1 format.
    pub const fn v1_with_dp(dp: DpAddress, ap: u8) -> Self {
        Self {
            dp,
            ap: ApAddress::V1(ap),
        }
    }

    /// Create a new `FullyQualifiedApAddress` belonging to the default debug port.
    pub const fn v2_with_default_dp(ap: ApV2Address) -> Self {
        Self {
            dp: DpAddress::Default,
            ap: ApAddress::V2(ap),
        }
    }

    /// Create a new `FullyQualifiedApAddress` belonging to the given debug port using Ap Address
    /// in the version 2 format.
    pub const fn v2_with_dp(dp: DpAddress, ap: ApV2Address) -> Self {
        Self {
            dp,
            ap: ApAddress::V2(ap),
        }
    }

    /// Returns the Debug port’s address.
    pub fn dp(&self) -> DpAddress {
        self.dp
    }

    /// Returns the Access Port address.
    pub fn ap(&self) -> &ApAddress {
        &self.ap
    }

    /// Returns the ap address if its version is 1.
    pub fn ap_v1(&self) -> Result<u8, ArmError> {
        if let ApAddress::V1(ap) = self.ap {
            Ok(ap)
        } else {
            Err(ArmError::WrongApVersion)
        }
    }

    /// Deconstruct an address into the DP and AP portions.
    pub fn deconstruct(self) -> (DpAddress, ApAddress) {
        (self.dp, self.ap)
    }
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
    /// Only the lowest 4 bits of the address are used. Bank switching is the caller's responsibility.
    fn raw_read_register(&mut self, address: RegisterAddress) -> Result<u32, ArmError>;

    /// Read multiple values from the same DAP register.
    ///
    /// If possible, this uses optimized read functions, otherwise it
    /// falls back to the `read_register` function.
    ///
    /// Only the lowest 4 bits of the address are used. Bank switching is the caller's responsibility.
    fn raw_read_block(
        &mut self,
        address: RegisterAddress,
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        for val in values {
            *val = self.raw_read_register(address)?;
        }

        Ok(())
    }

    /// Write a value to a DAP register.
    ///
    /// Only the lowest 4 bits of the address are used. Bank switching is the caller's responsibility.
    fn raw_write_register(&mut self, address: RegisterAddress, value: u32) -> Result<(), ArmError>;

    /// Write multiple values to the same DAP register.
    ///
    /// If possible, this uses optimized write functions, otherwise it
    /// falls back to the `write_register` function.
    ///
    /// Only bits 2 and 3 of the address are used. Bank switching is the caller's responsibility.
    fn raw_write_block(
        &mut self,
        address: RegisterAddress,
        values: &[u32],
    ) -> Result<(), ArmError> {
        for val in values {
            self.raw_write_register(address, *val)?;
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

    /// Configures the probe for JTAG use (specifying IR lengths of each DAP).
    fn configure_jtag(&mut self, _skip_scan: bool) -> Result<(), DebugProbeError> {
        Ok(())
    }

    /// Send a specific output sequence over JTAG.
    ///
    /// This can only be used for output, and should be used to generate
    /// the initial reset sequence, for example.
    fn jtag_sequence(&mut self, cycles: u8, tms: bool, tdi: u64) -> Result<(), DebugProbeError>;

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

    /// Inform the probe of the [`CoreStatus`] of the chip attached to the probe.
    fn core_status_notification(&mut self, state: CoreStatus) -> Result<(), DebugProbeError>;
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
    fn read_raw_dp_register(
        &mut self,
        dp: DpAddress,
        addr: DpRegisterAddress,
    ) -> Result<u32, ArmError>;

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
        addr: DpRegisterAddress,
        value: u32,
    ) -> Result<(), ArmError>;

    /// Read an Access Port register.
    ///
    /// # Note
    /// The address format depends on the AP type.
    /// * For APv2, the address is a register memory address within the AP memory space.
    /// * For APv1, the address is an 8-bit integer, where the highest 4 bits are interpreted as
    ///   the bank number, and implementations do bank switching if necessary.
    fn read_raw_ap_register(
        &mut self,
        ap: &FullyQualifiedApAddress,
        addr: u64,
    ) -> Result<u32, ArmError>;

    /// Read multiple values from the same Access Port register.
    ///
    /// If possible, this uses optimized read functions, otherwise it
    /// falls back to the `read_raw_ap_register` function.
    ///
    /// # Note
    /// The address format depends on the AP type.
    /// * For APv2, the address is a register memory address within the AP memory space.
    /// * For APv1, the address is an 8-bit integer, where the highest 4 bits are interpreted as
    ///   the bank number, and implementations do bank switching if necessary.
    fn read_raw_ap_register_repeated(
        &mut self,
        ap: &FullyQualifiedApAddress,
        addr: u64,
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        for val in values {
            *val = self.read_raw_ap_register(ap, addr)?;
        }
        Ok(())
    }

    /// Write an AP register.
    ///
    /// # Note
    /// The address format depends on the AP type.
    /// * For APv2, the address is a register memory address within the AP memory space.
    /// * For APv1, the address is an 8-bit integer, where the highest 4 bits are interpreted as
    ///   the bank number, and implementations do bank switching if necessary.
    fn write_raw_ap_register(
        &mut self,
        ap: &FullyQualifiedApAddress,
        addr: u64,
        value: u32,
    ) -> Result<(), ArmError>;

    /// Write multiple values to the same Access Port register.
    ///
    /// If possible, this uses optimized write functions, otherwise it
    /// falls back to the `write_raw_ap_register` function.
    ///
    /// # Note
    /// The address format depends on the AP type.
    /// * For APv2, the address is a register memory address within the AP memory space.
    /// * For APv1, the address is an 8-bit integer, where the highest 4 bits are interpreted as
    ///   the bank number, and implementations do bank switching if necessary.
    fn write_raw_ap_register_repeated(
        &mut self,
        ap: &FullyQualifiedApAddress,
        addr: u64,
        values: &[u32],
    ) -> Result<(), ArmError> {
        for val in values {
            self.write_raw_ap_register(ap, addr, *val)?;
        }
        Ok(())
    }

    /// Flush any outstanding operations.
    ///
    /// For performance, debug probe implementations may choose to batch writes;
    /// to assure that any such batched writes have in fact been issued, `flush`
    /// can be called.  Takes no arguments, but may return failure if a batched
    /// operation fails.
    fn flush(&mut self) -> Result<(), ArmError> {
        Ok(())
    }

    /// Gain access to the Probe that implements this trait
    fn try_dap_probe(&self) -> Option<&dyn DapProbe>;

    /// Gain mutable access to the Probe that implements this trait
    fn try_dap_probe_mut(&mut self) -> Option<&mut dyn DapProbe>;
}
