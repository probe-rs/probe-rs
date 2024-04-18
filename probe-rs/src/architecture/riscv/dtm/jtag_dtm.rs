//! Debug Transport Module (DTM) handling
//!
//! The DTM is responsible for access to the debug module.
//! Currently, only JTAG is supported.
use crate::architecture::riscv::dtm::dtm_access::DtmAccess;
use bitfield::bitfield;
use std::time::{Duration, Instant};

use crate::architecture::riscv::communication_interface::RiscvError;
use crate::probe::{
    CommandResult, DeferredResultIndex, DeferredResultSet, JTAGAccess, JtagCommandQueue,
    JtagWriteCommand,
};
use crate::probe::{DebugProbe, DebugProbeError, Probe};

/// Access to the Debug Transport Module (DTM),
/// which is used to communicate with the RISC-V debug module.
#[derive(Debug)]
pub struct JtagDtm {
    pub probe: Box<dyn JTAGAccess>,

    queued_commands: JtagCommandQueue,
    jtag_results: DeferredResultSet,

    /// Number of address bits in the DMI register
    abits: u32,
}

impl JtagDtm {
    pub fn new(probe: Box<dyn JTAGAccess>) -> Self {
        Self {
            probe,
            abits: 0,
            queued_commands: JtagCommandQueue::new(),
            jtag_results: DeferredResultSet::new(),
        }
    }

    fn transform_dmi_result(response_bytes: Vec<u8>) -> Result<u32, DmiOperationStatus> {
        let response_value: u128 = response_bytes.iter().enumerate().fold(0, |acc, elem| {
            let (byte_offset, value) = elem;
            acc + ((*value as u128) << (8 * byte_offset))
        });

        // Verify that the transfer was ok
        let op = (response_value & DMI_OP_MASK) as u8;

        if op != 0 {
            // We masked out two bits, parse(op) always works on values 0, 1, 2 and 3
            return Err(DmiOperationStatus::parse(op).expect("INVALID DMI OP status"));
        }

        Ok((response_value >> 2) as u32)
    }

    /// Perform an access to the dmi register of the JTAG Transport module.
    ///
    /// Every access both writes and reads from the register, which means a value is always
    /// returned. The `op` is checked for errors, and if it is not equal to zero, an error is returned.
    fn dmi_register_access(
        &mut self,
        op: DmiOperation,
    ) -> Result<Result<u32, DmiOperationStatus>, DebugProbeError> {
        let bytes = op.to_byte_batch();

        let bit_size = self.abits + DMI_ADDRESS_BIT_OFFSET;

        self.probe
            .write_register(DMI_ADDRESS, &bytes, bit_size)
            .map(Self::transform_dmi_result)
    }

    fn schedule_dmi_register_access(
        &mut self,
        op: DmiOperation,
    ) -> Result<DeferredResultIndex, RiscvError> {
        let bytes = op.to_byte_batch();

        let bit_size = self.abits + DMI_ADDRESS_BIT_OFFSET;

        Ok(self.queued_commands.schedule(JtagWriteCommand {
            address: DMI_ADDRESS,
            data: bytes.to_vec(),
            transform: |_, result| {
                Self::transform_dmi_result(result)
                    .map(CommandResult::U32)
                    .map_err(|e| crate::Error::Riscv(e.map_as_err().unwrap_err()))
            },
            len: bit_size,
        }))
    }

    fn dmi_register_access_with_timeout(
        &mut self,
        op: DmiOperation,
        timeout: Duration,
    ) -> Result<u32, RiscvError> {
        let start_time = Instant::now();

        self.execute()?;

        loop {
            match self.dmi_register_access(op)? {
                Ok(result) => return Ok(result),
                Err(DmiOperationStatus::RequestInProgress) => {
                    // Operation still in progress, reset dmi status and try again.
                    self.clear_error_state()?;
                    self.probe
                        .set_idle_cycles(self.probe.idle_cycles().saturating_add(1));
                }
                Err(e) => return Err(e.map_as_err().unwrap_err()),
            };

            if start_time.elapsed() > timeout {
                return Err(RiscvError::Timeout);
            }
        }
    }
}

impl DtmAccess for JtagDtm {
    fn init(&mut self) -> Result<(), RiscvError> {
        self.probe.tap_reset()?;
        let dtmcs_raw = self.probe.read_register(DTMCS_ADDRESS, DTMCS_WIDTH)?;

        let raw_dtmcs = u32::from_le_bytes((&dtmcs_raw[..]).try_into().unwrap());

        if raw_dtmcs == 0 {
            return Err(RiscvError::NoRiscvTarget);
        }

        let dtmcs = Dtmcs(raw_dtmcs);

        tracing::debug!("{:?}", dtmcs);

        let abits = dtmcs.abits();
        let idle_cycles = dtmcs.idle();

        if dtmcs.version() != 1 {
            return Err(RiscvError::UnsupportedDebugTransportModuleVersion(
                dtmcs.version() as u8,
            ));
        }

        // Setup the number of idle cycles between JTAG accesses
        self.probe.set_idle_cycles(idle_cycles as u8);
        self.abits = abits;

        Ok(())
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        self.probe.target_reset_assert()
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        self.probe.target_reset_deassert()
    }

    fn clear_error_state(&mut self) -> Result<(), RiscvError> {
        let mut dtmcs = Dtmcs(0);

        dtmcs.set_dmireset(true);

        let Dtmcs(reg_value) = dtmcs;

        let bytes = reg_value.to_le_bytes();

        self.probe
            .write_register(DTMCS_ADDRESS, &bytes, DTMCS_WIDTH)?;

        Ok(())
    }

    fn read_deferred_result(
        &mut self,
        index: DeferredResultIndex,
    ) -> Result<CommandResult, RiscvError> {
        match self.jtag_results.take(index) {
            Ok(result) => Ok(result),
            Err(index) => {
                self.execute()?;
                // We can lose data if `execute` fails.
                self.jtag_results
                    .take(index)
                    .map_err(|_| RiscvError::BatchedResultNotAvailable)
            }
        }
    }

    fn close(self: Box<Self>) -> Probe {
        Probe::from_attached_probe(self.probe.into_probe())
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self.probe.into_probe()
    }

    fn execute(&mut self) -> Result<(), RiscvError> {
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
                            RiscvError::DtmOperationInProcess => {
                                self.clear_error_state()?;

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

    fn schedule_write(
        &mut self,
        address: u64,
        value: u32,
    ) -> Result<Option<DeferredResultIndex>, RiscvError> {
        self.schedule_dmi_register_access(DmiOperation::Write { address, value })
            .map(Some)
    }

    fn schedule_read(&mut self, address: u64) -> Result<DeferredResultIndex, RiscvError> {
        // Prepare the read by sending a read request with the register address
        self.schedule_dmi_register_access(DmiOperation::Read { address })?;

        // Read back the response from the previous request.
        self.schedule_dmi_register_access(DmiOperation::NoOp)
    }

    fn read_with_timeout(&mut self, address: u64, timeout: Duration) -> Result<u32, RiscvError> {
        // Prepare the read by sending a read request with the register address
        self.schedule_dmi_register_access(DmiOperation::Read { address })?;

        self.dmi_register_access_with_timeout(DmiOperation::NoOp, timeout)
    }

    fn write_with_timeout(
        &mut self,
        address: u64,
        value: u32,
        timeout: Duration,
    ) -> Result<Option<u32>, RiscvError> {
        self.dmi_register_access_with_timeout(DmiOperation::Write { address, value }, timeout)
            .map(Some)
    }

    fn read_idcode(&mut self) -> Result<Option<u32>, DebugProbeError> {
        let value = self.probe.read_register(0x1, 32)?;

        Ok(Some(u32::from_le_bytes((&value[..]).try_into().unwrap())))
    }
}

#[derive(Copy, Clone, Debug)]
pub enum DmiOperation {
    NoOp,
    Read { address: u64 },
    Write { address: u64, value: u32 },
}

impl DmiOperation {
    fn opcode(&self) -> u8 {
        match self {
            DmiOperation::NoOp => 0,
            DmiOperation::Read { .. } => 1,
            DmiOperation::Write { .. } => 2,
        }
    }

    pub fn to_byte_batch(self: DmiOperation) -> [u8; 16] {
        let (opcode, address, value): (u128, u128, u128) = match self {
            DmiOperation::NoOp => (self.opcode() as u128, 0, 0),
            DmiOperation::Read { address } => (self.opcode() as u128, address as u128, 0),
            DmiOperation::Write { address, value } => {
                (self.opcode() as u128, address as u128, value as u128)
            }
        };
        let register_value: u128 =
            (address << DMI_ADDRESS_BIT_OFFSET) | (value << DMI_VALUE_BIT_OFFSET) | opcode;

        register_value.to_le_bytes()
    }
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
    pub fn map_as_err(self) -> Result<(), RiscvError> {
        match self {
            DmiOperationStatus::Ok => Ok(()),
            DmiOperationStatus::Reserved => unimplemented!("Reserved."),
            DmiOperationStatus::OperationFailed => Err(RiscvError::DtmOperationFailed),
            DmiOperationStatus::RequestInProgress => Err(RiscvError::DtmOperationInProcess),
        }
    }
}

impl DmiOperationStatus {
    pub(crate) fn parse(value: u8) -> Option<Self> {
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

bitfield! {
    /// The `dtmcs` register is
    pub struct Dtmcs(u32);
    impl Debug;

    pub _, set_dmihardreset: 17;
    pub _, set_dmireset: 16;
    pub idle, _: 14, 12;
    pub dmistat, _: 11,10;
    pub abits, _: 9,4;
    pub version, _: 3,0;
}
