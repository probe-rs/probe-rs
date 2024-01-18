//! Debug Transport Module (DTM) handling
//!
//! The DTM is responsible for access to the debug module.
//! Currently, only JTAG is supported.
use std::{
    convert::TryInto,
    time::{Duration, Instant},
};

use bitfield::bitfield;

use super::communication_interface::RiscvError;
use crate::{
    probe::{
        CommandResult, DeferredResultIndex, DeferredResultSet, JTAGAccess, JtagCommandQueue,
        JtagWriteCommand,
    },
    DebugProbeError,
};

/// Access to the Debug Transport Module (DTM),
/// which is used to communicate with the RISC-V debug module.
#[derive(Debug)]
pub struct Dtm {
    pub probe: Box<dyn JTAGAccess>,

    queued_commands: JtagCommandQueue,
    jtag_results: DeferredResultSet,

    /// Number of address bits in the DMI register
    abits: u32,
}

impl Dtm {
    pub fn new(mut probe: Box<dyn JTAGAccess>) -> Result<Self, (Box<dyn JTAGAccess>, RiscvError)> {
        let dtmcs_raw = match probe.read_register(DTMCS_ADDRESS, DTMCS_WIDTH) {
            Ok(value) => value,
            Err(e) => return Err((probe, e.into())),
        };

        let raw_dtmcs = u32::from_le_bytes((&dtmcs_raw[..]).try_into().unwrap());

        if raw_dtmcs == 0 {
            return Err((probe, RiscvError::NoRiscvTarget));
        }

        let dtmcs = Dtmcs(raw_dtmcs);

        tracing::debug!("Dtmcs: {:?}", dtmcs);

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

        Ok(Self {
            probe,
            abits,
            queued_commands: JtagCommandQueue::new(),
            jtag_results: DeferredResultSet::new(),
        })
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

    pub fn read_deferred_result(
        &mut self,
        index: DeferredResultIndex,
    ) -> Result<CommandResult, RiscvError> {
        let result = match self.jtag_results.take(index) {
            Ok(result) => result,
            Err(index) => {
                self.execute()?;
                self.jtag_results.take(index).expect("This is a bug")
            }
        };

        Ok(result)
    }

    pub fn execute(&mut self) -> Result<(), RiscvError> {
        let mut cmds = std::mem::take(&mut self.queued_commands);

        while !cmds.is_empty() {
            match self.probe.write_register_batch(&cmds) {
                Ok(r) => {
                    self.jtag_results.merge_from(r);
                    return Ok(());
                }
                Err(e) => match e.error {
                    crate::Error::Riscv(ae) => {
                        match ae {
                            RiscvError::DmiTransfer(DmiOperationStatus::RequestInProgress) => {
                                self.reset()?;

                                // queue up the remaining commands when we retry
                                cmds.consume(e.results.len());
                                self.jtag_results.merge_from(e.results);

                                self.probe.set_idle_cycles(self.probe.idle_cycles() + 1);
                            }
                            _ => return Err(ae),
                        }
                    }
                    crate::Error::Probe(err) => return Err(err.into()),
                    _other => unreachable!(),
                },
            }
        }

        Ok(())
    }

    fn transform_dmi_result(response_bytes: Vec<u8>) -> Result<u32, DmiOperationStatus> {
        let response_value: u128 = response_bytes.iter().enumerate().fold(0, |acc, elem| {
            let (byte_offset, value) = elem;
            acc + ((*value as u128) << (8 * byte_offset))
        });

        // Verify that the transfer was ok
        let op = (response_value & DMI_OP_MASK) as u8;

        if op != 0 {
            return Err(DmiOperationStatus::parse(op).expect("INVALID DMI OP status"));
        }

        Ok((response_value >> 2) as u32)
    }

    fn dmi_command(address: u64, value: u32, op: DmiOperation) -> [u8; 16] {
        let register_value: u128 = ((address as u128) << DMI_ADDRESS_BIT_OFFSET)
            | ((value as u128) << DMI_VALUE_BIT_OFFSET)
            | op as u128;

        register_value.to_le_bytes()
    }

    pub fn schedule_dmi_register_access(
        &mut self,
        address: u64,
        value: u32,
        op: DmiOperation,
    ) -> Result<DeferredResultIndex, RiscvError> {
        let bytes = Self::dmi_command(address, value, op);

        let bit_size = self.abits + DMI_ADDRESS_BIT_OFFSET;

        Ok(self.queued_commands.schedule(JtagWriteCommand {
            address: DMI_ADDRESS,
            data: bytes.to_vec(),
            transform: |result| {
                Self::transform_dmi_result(result)
                    .map(CommandResult::U32)
                    .map_err(|status| crate::Error::Riscv(RiscvError::DmiTransfer(status)))
            },
            len: bit_size,
        }))
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
        let bytes = Self::dmi_command(address, value, op);

        let bit_size = self.abits + DMI_ADDRESS_BIT_OFFSET;

        self.probe
            .write_register(DMI_ADDRESS, &bytes, bit_size)
            .map(Self::transform_dmi_result)
    }

    /// Read or write the `dmi` register. If a busy value is returned, the access is
    /// retried until the transfer either succeeds, or the timeout expires.
    pub fn dmi_register_access_with_timeout(
        &mut self,
        address: u64,
        value: u32,
        op: DmiOperation,
        timeout: Duration,
    ) -> Result<u32, RiscvError> {
        let start_time = Instant::now();

        self.execute()?;

        loop {
            match self.dmi_register_access(address, value, op)? {
                Ok(result) => return Ok(result),
                Err(DmiOperationStatus::RequestInProgress) => {
                    // Operation still in progress, reset dmi status and try again.
                    self.reset()?;
                    self.probe
                        .set_idle_cycles(self.probe.idle_cycles().saturating_add(1));
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
