use std::{
    convert::TryInto,
    time::{Duration, Instant},
};

use bitfield::bitfield;

use super::communication_interface::RiscvError;
use crate::{
    probe::{CommandResults, DeferredCommandResult, JTAGAccess},
    DebugProbeError,
};

///! Debug Transport Module (DTM) handling
///!
///! The DTM is responsible for access to the debug module.
///! Currently, only JTAG is supported.

/// Access to the Debug Transport Module (DTM),
/// which is used to communicate with the RISCV debug module.
#[derive(Debug)]
pub struct Dtm {
    pub probe: Box<dyn JTAGAccess>,

    /// Number of address bits in the DMI register
    abits: u32,
}

impl Dtm {
    pub fn new(mut probe: Box<dyn JTAGAccess>) -> Result<Self, (Box<dyn JTAGAccess>, RiscvError)> {
        let dtmcs_raw = match probe.read_register(DTMCS_ADDRESS, DTMCS_WIDTH) {
            Ok(value) => value,
            Err(e) => return Err((probe, e.into())),
        };

        let dtmcs = Dtmcs(u32::from_le_bytes((&dtmcs_raw[..]).try_into().unwrap()));

        log::debug!("Dtmcs: {:?}", dtmcs);

        let abits = dtmcs.abits();
        let idle_cycles = dtmcs.idle();

        if dtmcs.version() != 1 {
            return Err((
                probe,
                RiscvError::UnsupportedDebugTransportModuleVersion(dtmcs.version() as u8),
            ));
        }

        // Setup the number of idle cycles between JTAG accesses
        probe.set_idle_cycles(idle_cycles as u8);

        Ok(Self { probe, abits })
    }

    pub fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        self.probe.target_reset_deassert()
    }

    pub fn read_idcode(&mut self) -> Result<u32, DebugProbeError> {
        let value = self.probe.read_register(0x1, 32)?;

        Ok(u32::from_le_bytes((&value[..]).try_into().unwrap()))
    }

    /// Clear the sticky error state (field *op* in the DMI register)
    pub fn reset(&mut self) -> Result<(), RiscvError> {
        let mut dtmcs = Dtmcs(0);

        dtmcs.set_dmireset(true);

        let Dtmcs(reg_value) = dtmcs;

        let bytes = reg_value.to_le_bytes();

        self.probe
            .write_register(DTMCS_ADDRESS, &bytes, DTMCS_WIDTH)?;

        Ok(())
    }

    pub fn execute(&mut self) -> Result<Box<dyn CommandResults>, DebugProbeError> {
        self.probe.execute()
    }

    pub fn schedule_dmi_register_access(
        &mut self,
        address: u64,
        value: u32,
        op: DmiOperation,
    ) -> Result<Box<dyn DeferredCommandResult>, DebugProbeError> {
        let register_value: u128 = ((address as u128) << DMI_ADDRESS_BIT_OFFSET)
            | ((value as u128) << DMI_VALUE_BIT_OFFSET)
            | op as u128;

        let bytes = register_value.to_le_bytes();

        let bit_size = self.abits + DMI_ADDRESS_BIT_OFFSET;

        self.probe
            .schedule_write_register(DMI_ADDRESS, &bytes, bit_size, |response_bytes| {
                let response_value: u128 =
                    response_bytes.iter().enumerate().fold(0, |acc, elem| {
                        let (byte_offset, value) = elem;
                        acc + ((*value as u128) << (8 * byte_offset))
                    });

                // Verify that the transfer was ok
                let op = (response_value & DMI_OP_MASK) as u8;

                if op != 0 {
                    return Err(DebugProbeError::ProbeSpecific(Box::new(
                        std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("Operation Status = {:?}", DmiOperationStatus::parse(op)),
                        ),
                    )));
                }

                let value = (response_value >> 2) as u32;
                Ok(value)
            })
    }

    /// Perform an access to the dmi register of the JTAG Transport module.
    ///
    /// Every access both writes and reads from the register, which means a value is always
    /// returned. The `op` is checked for errors, and if it is not equal to zero, an error is returned.
    fn dmi_register_access(
        &mut self,
        address: u64,
        value: u32,
        op: DmiOperation,
    ) -> Result<Result<u32, DmiOperationStatus>, DebugProbeError> {
        let register_value: u128 = ((address as u128) << DMI_ADDRESS_BIT_OFFSET)
            | ((value as u128) << DMI_VALUE_BIT_OFFSET)
            | op as u128;

        let bytes = register_value.to_le_bytes();

        let bit_size = self.abits + DMI_ADDRESS_BIT_OFFSET;

        let response_bytes = self.probe.write_register(DMI_ADDRESS, &bytes, bit_size)?;

        let response_value: u128 = response_bytes.iter().enumerate().fold(0, |acc, elem| {
            let (byte_offset, value) = elem;
            acc + ((*value as u128) << (8 * byte_offset))
        });

        // Verify that the transfer was ok
        let op = (response_value & DMI_OP_MASK) as u8;

        if op != 0 {
            return Ok(Err(DmiOperationStatus::parse(op).unwrap()));
        }

        let value = (response_value >> 2) as u32;

        Ok(Ok(value))
    }

    /// Read or write the `dmi` register. If a busy value is rerurned, the access is
    /// retried until the transfer either succeeds, or the tiemout expires.
    pub fn dmi_register_access_with_timeout(
        &mut self,
        address: u64,
        value: u32,
        op: DmiOperation,
        timeout: Duration,
    ) -> Result<u32, RiscvError> {
        let start_time = Instant::now();

        loop {
            match self.dmi_register_access(address, value, op)? {
                Ok(result) => return Ok(result),
                Err(DmiOperationStatus::RequestInProgress) => {
                    // Operation still in progress, reset dmi status and try again.
                    self.reset()?;
                }
                Err(e) => return Err(RiscvError::DmiTransfer(e)),
            }

            if start_time.elapsed() > timeout {
                return Err(RiscvError::Timeout);
            }
        }
    }
}

bitfield! {
    /// The `dtmcs` register is
    struct Dtmcs(u32);
    impl Debug;

    _, set_dmihardreset: 17;
    _, set_dmireset: 16;
    idle, _: 14, 12;
    dmistat, _: 11,10;
    abits, _: 9,4;
    version, _: 3,0;
}

/// Address of the `dtmcs` JTAG register.
const DTMCS_ADDRESS: u32 = 0x10;

/// Width of the `dtmcs` JTAG register.
const DTMCS_WIDTH: u32 = 32;

/// Address of the `dmi` JTAG register
const DMI_ADDRESS: u32 = 0x11;

/// Offset of the `address` field in the `dmi` JTAG register.
const DMI_ADDRESS_BIT_OFFSET: u32 = 34;

/// Offset of the `value` field in the `dmi` JTAG register.
const DMI_VALUE_BIT_OFFSET: u32 = 2;

const DMI_OP_MASK: u128 = 0x3;

#[derive(Copy, Clone, Debug)]
pub enum DmiOperation {
    NoOp = 0,
    Read = 1,
    Write = 2,
    _Reserved = 3,
}

/// Possible return values in the op field of
/// the dmi register.
#[derive(Debug)]
pub enum DmiOperationStatus {
    Ok = 0,
    Reserved = 1,
    OperationFailed = 2,
    RequestInProgress = 3,
}

impl DmiOperationStatus {
    fn parse(value: u8) -> Option<Self> {
        use DmiOperationStatus::*;

        let status = match value {
            0 => Ok,
            1 => Reserved,
            2 => OperationFailed,
            3 => RequestInProgress,
            _ => return None,
        };

        Some(status)
    }
}
