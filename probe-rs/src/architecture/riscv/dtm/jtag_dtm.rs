//! Debug Transport Module (DTM) handling
//!
//! The DTM is responsible for access to the debug module.
//! Currently, only JTAG is supported.

use bitfield::bitfield;
use bitvec::field::BitField;
use bitvec::slice::BitSlice;
use std::time::{Duration, Instant};

use crate::architecture::arm::ArmError;
use crate::architecture::riscv::communication_interface::{
    RiscvCommunicationInterface, RiscvDebugInterfaceState, RiscvError, RiscvInterfaceBuilder,
};
use crate::architecture::riscv::dtm::dtm_access::{DmAddress, DtmAccess};
use crate::error::Error;
use crate::probe::{
    CommandQueue, CommandResult, DeferredResultIndex, DeferredResultSet, JtagAccess, JtagCommand,
    JtagWriteCommand,
};
use crate::probe::{DebugProbeError, ShiftDrCommand};

#[derive(Debug, Default)]
struct DtmState {
    queued_commands: CommandQueue<JtagCommand>,
    jtag_results: DeferredResultSet<CommandResult>,

    /// Number of address bits in the DMI register
    abits: u32,
}

/// Object that can be used to build a RISC-V DTM interface
/// from a JTAG transport.
pub struct JtagDtmBuilder<'f>(&'f mut dyn JtagAccess);

impl<'f> JtagDtmBuilder<'f> {
    /// Create a new DTM Builder via a JTAG transport.
    pub fn new(probe: &'f mut dyn JtagAccess) -> Self {
        Self(probe)
    }
}

impl<'probe> RiscvInterfaceBuilder<'probe> for JtagDtmBuilder<'probe> {
    fn create_state(&self) -> RiscvDebugInterfaceState {
        let dtm_state = DtmState::default();

        // We don't specify a memory access method here.
        RiscvDebugInterfaceState::new(Box::new(dtm_state), None)
    }

    fn attach<'state>(
        self: Box<Self>,
        state: &'state mut RiscvDebugInterfaceState,
    ) -> Result<RiscvCommunicationInterface<'state>, DebugProbeError>
    where
        'probe: 'state,
    {
        let dtm_state = state.dtm_state.downcast_mut::<DtmState>().unwrap();

        Ok(RiscvCommunicationInterface::new(
            Box::new(JtagDtm::new(self.0, dtm_state)),
            &mut state.interface_state,
        ))
    }

    fn attach_tunneled<'state>(
        self: Box<Self>,
        tunnel_ir_id: u32,
        tunnel_ir_width: u32,
        state: &'state mut RiscvDebugInterfaceState,
    ) -> Result<RiscvCommunicationInterface<'state>, DebugProbeError>
    where
        'probe: 'state,
    {
        let dtm_state = state.dtm_state.downcast_mut::<DtmState>().unwrap();

        Ok(RiscvCommunicationInterface::new(
            Box::new(TunneledJtagDtm::new(
                self.0,
                tunnel_ir_id,
                tunnel_ir_width,
                dtm_state,
            )),
            &mut state.interface_state,
        ))
    }
}

/// Access to the Debug Transport Module (DTM),
/// which is used to communicate with the RISC-V debug module.
#[derive(Debug)]
pub struct JtagDtm<'probe> {
    pub probe: &'probe mut dyn JtagAccess,
    state: &'probe mut DtmState,
}

impl<'probe> JtagDtm<'probe> {
    fn new(probe: &'probe mut dyn JtagAccess, state: &'probe mut DtmState) -> Self {
        Self { probe, state }
    }

    fn transform_dmi_result(response_bits: &BitSlice) -> Result<u32, DmiOperationStatus> {
        let response_value = response_bits.load_le::<u128>();

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

        let bit_size = self.state.abits + DMI_ADDRESS_BIT_OFFSET;

        self.probe
            .write_register(DMI_ADDRESS, &bytes, bit_size)
            .map(|bits| Self::transform_dmi_result(&bits))
    }

    fn schedule_dmi_register_access(
        &mut self,
        op: DmiOperation,
    ) -> Result<DeferredResultIndex, RiscvError> {
        let bytes = op.to_byte_batch();

        let bit_size = self.state.abits + DMI_ADDRESS_BIT_OFFSET;

        Ok(self.state.queued_commands.schedule(JtagWriteCommand {
            address: DMI_ADDRESS,
            data: bytes.to_vec(),
            transform: |_, result| {
                Self::transform_dmi_result(result)
                    .map(CommandResult::U32)
                    .map_err(|e| Error::Riscv(e.map_as_err().unwrap_err()))
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
                        .set_idle_cycles(self.probe.idle_cycles().saturating_add(1))?;
                }
                Err(e) => return Err(e.map_as_err().unwrap_err()),
            };

            if start_time.elapsed() > timeout {
                return Err(RiscvError::Timeout);
            }
        }
    }
}

impl DtmAccess for JtagDtm<'_> {
    fn init(&mut self) -> Result<(), RiscvError> {
        self.probe.tap_reset()?;
        let dtmcs_raw = self.probe.read_register(DTMCS_ADDRESS, DTMCS_WIDTH)?;

        let raw_dtmcs = dtmcs_raw.load_le::<u32>();

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
        self.probe.set_idle_cycles(idle_cycles as u8)?;
        self.state.abits = abits;

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
        match self.state.jtag_results.take(index) {
            Ok(result) => Ok(result),
            Err(index) => {
                self.execute()?;
                // We can lose data if `execute` fails.
                self.state
                    .jtag_results
                    .take(index)
                    .map_err(|_| RiscvError::BatchedResultNotAvailable)
            }
        }
    }

    fn execute(&mut self) -> Result<(), RiscvError> {
        let mut cmds = std::mem::take(&mut self.state.queued_commands);

        while !cmds.is_empty() {
            match self.probe.write_register_batch(&cmds) {
                Ok(r) => {
                    self.state.jtag_results.merge_from(r);
                    return Ok(());
                }
                Err(e) => match e.error {
                    Error::Riscv(RiscvError::DtmOperationInProcess) => {
                        self.clear_error_state()?;

                        // queue up the remaining commands when we retry
                        cmds.consume(e.results.len());
                        self.state.jtag_results.merge_from(e.results);

                        self.probe
                            .set_idle_cycles(self.probe.idle_cycles().saturating_add(1))?;
                    }
                    Error::Riscv(error) => return Err(error),
                    Error::Probe(error) => return Err(error.into()),
                    _other => unreachable!(),
                },
            }
        }

        Ok(())
    }

    fn schedule_write(
        &mut self,
        address: DmAddress,
        value: u32,
    ) -> Result<Option<DeferredResultIndex>, RiscvError> {
        self.schedule_dmi_register_access(DmiOperation::Write {
            address: address.0,
            value,
        })
        .map(Some)
    }

    fn schedule_read(&mut self, address: DmAddress) -> Result<DeferredResultIndex, RiscvError> {
        // Prepare the read by sending a read request with the register address
        self.schedule_dmi_register_access(DmiOperation::Read { address: address.0 })?;

        // Read back the response from the previous request.
        self.schedule_dmi_register_access(DmiOperation::NoOp)
    }

    fn read_with_timeout(
        &mut self,
        address: DmAddress,
        timeout: Duration,
    ) -> Result<u32, RiscvError> {
        // Prepare the read by sending a read request with the register address
        self.schedule_dmi_register_access(DmiOperation::Read { address: address.0 })?;

        self.dmi_register_access_with_timeout(DmiOperation::NoOp, timeout)
    }

    fn write_with_timeout(
        &mut self,
        address: DmAddress,
        value: u32,
        timeout: Duration,
    ) -> Result<Option<u32>, RiscvError> {
        self.dmi_register_access_with_timeout(
            DmiOperation::Write {
                address: address.0,
                value,
            },
            timeout,
        )
        .map(Some)
    }

    fn read_idcode(&mut self) -> Result<Option<u32>, DebugProbeError> {
        let value = self.probe.read_register(0x1, 32)?;

        Ok(Some(value.load_le::<u32>()))
    }

    fn memory_interface<'m>(
        &'m mut self,
    ) -> Result<&'m mut dyn crate::MemoryInterface<ArmError>, DebugProbeError> {
        todo!()
    }
}

/// Access to the tunneled Debug Transport Module (DTM),
/// which is used to communicate with the RISC-V debug module.
///
/// The protocol was originally created by SiFive for their IP, but many others implement it. For
/// reference, see the `riscv use_bscan_tunnel` command of riscv-openocd.
///
/// Tunneled DR scan:
/// 1. Select IR (0) or DR (1): 1 bit
/// 2. Width of tunneled scan: 7 bits
/// 3. Tunneled scan bits: width + 1 bits
/// 4. Set tunnel to idle: 3 zero bits
#[derive(Debug)]
pub struct TunneledJtagDtm<'probe> {
    pub probe: &'probe mut dyn JtagAccess,
    state: &'probe mut DtmState,
    select_dtmcs: JtagWriteCommand,
    select_dmi: JtagWriteCommand,
}

impl<'probe> TunneledJtagDtm<'probe> {
    fn new(
        probe: &'probe mut dyn JtagAccess,
        tunnel_ir_id: u32,
        tunnel_ir_width: u32,
        state: &'probe mut DtmState,
    ) -> Self {
        Self {
            probe,
            state,
            select_dtmcs: tunnel_select_command(tunnel_ir_id, tunnel_ir_width, DTMCS_ADDRESS),
            select_dmi: tunnel_select_command(tunnel_ir_id, tunnel_ir_width, DMI_ADDRESS),
        }
    }

    fn write_dtmcs(&mut self, data: u32) -> Result<u32, RiscvError> {
        self.probe.write_register(
            self.select_dtmcs.address,
            &self.select_dtmcs.data,
            self.select_dtmcs.len,
        )?;
        let cmd = tunnel_dtmcs_command(data);
        let result = self
            .probe
            .write_dr(&cmd.data, cmd.len)
            .map(|r| (cmd.transform)(&cmd, &r))?;
        match result {
            Ok(CommandResult::U32(d)) => Ok(d),
            Err(crate::Error::Probe(e)) => Err(e.into()),
            _ => Err(RiscvError::DtmOperationFailed),
        }
    }

    fn transform_tunneled_dr_result(response_bits: &BitSlice) -> &BitSlice {
        &response_bits[4..]
    }

    fn dmi_register_access(
        &mut self,
        op: DmiOperation,
    ) -> Result<Result<u32, DmiOperationStatus>, DebugProbeError> {
        self.probe.write_register(
            self.select_dmi.address,
            &self.select_dmi.data,
            self.select_dmi.len,
        )?;

        let dmi_bits = self.state.abits + DMI_ADDRESS_BIT_OFFSET;
        let (bit_size, bytes) = op.to_tunneled_byte_batch(dmi_bits);
        let result = self.probe.write_dr(&bytes, bit_size)?;
        let tunneled_result = Self::transform_tunneled_dr_result(&result);
        Ok(JtagDtm::transform_dmi_result(tunneled_result))
    }

    fn schedule_dmi_register_access(
        &mut self,
        op: DmiOperation,
    ) -> Result<DeferredResultIndex, RiscvError> {
        self.state.queued_commands.schedule(self.select_dmi.clone());

        let dmi_bits = self.state.abits + DMI_ADDRESS_BIT_OFFSET;
        let (bit_size, bytes) = op.to_tunneled_byte_batch(dmi_bits);

        Ok(self.state.queued_commands.schedule(ShiftDrCommand {
            data: bytes.to_vec(),
            transform: |_, raw_result| {
                let result = Self::transform_tunneled_dr_result(raw_result);
                JtagDtm::transform_dmi_result(result)
                    .map(CommandResult::U32)
                    .map_err(|e| Error::Riscv(e.map_as_err().unwrap_err()))
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
                        .set_idle_cycles(self.probe.idle_cycles().saturating_add(1))?;
                }
                Err(e) => return Err(e.map_as_err().unwrap_err()),
            };

            if start_time.elapsed() > timeout {
                return Err(RiscvError::Timeout);
            }
        }
    }
}

impl DtmAccess for TunneledJtagDtm<'_> {
    fn init(&mut self) -> Result<(), RiscvError> {
        self.probe.tap_reset()?;
        let raw_dtmcs = self.write_dtmcs(0)?;

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
        self.probe.set_idle_cycles(idle_cycles as u8)?;
        self.state.abits = abits;

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

        self.write_dtmcs(reg_value)?;

        Ok(())
    }

    fn read_deferred_result(
        &mut self,
        index: DeferredResultIndex,
    ) -> Result<CommandResult, RiscvError> {
        match self.state.jtag_results.take(index) {
            Ok(result) => Ok(result),
            Err(index) => {
                self.execute()?;
                // We can lose data if `execute` fails.
                self.state
                    .jtag_results
                    .take(index)
                    .map_err(|_| RiscvError::BatchedResultNotAvailable)
            }
        }
    }

    fn execute(&mut self) -> Result<(), RiscvError> {
        let mut cmds = std::mem::take(&mut self.state.queued_commands);

        while !cmds.is_empty() {
            match self.probe.write_register_batch(&cmds) {
                Ok(r) => {
                    self.state.jtag_results.merge_from(r);
                    return Ok(());
                }
                Err(e) => match e.error {
                    Error::Riscv(RiscvError::DtmOperationInProcess) => {
                        self.clear_error_state()?;

                        // queue up the remaining commands when we retry
                        cmds.consume(e.results.len());
                        self.state.jtag_results.merge_from(e.results);

                        self.probe
                            .set_idle_cycles(self.probe.idle_cycles().saturating_add(1))?;
                    }
                    Error::Riscv(error) => return Err(error),
                    Error::Probe(error) => return Err(error.into()),
                    _other => unreachable!(),
                },
            }
        }

        Ok(())
    }

    fn schedule_write(
        &mut self,
        address: DmAddress,
        value: u32,
    ) -> Result<Option<DeferredResultIndex>, RiscvError> {
        self.schedule_dmi_register_access(DmiOperation::Write {
            address: address.0,
            value,
        })
        .map(Some)
    }

    fn schedule_read(&mut self, address: DmAddress) -> Result<DeferredResultIndex, RiscvError> {
        // Prepare the read by sending a read request with the register address
        self.schedule_dmi_register_access(DmiOperation::Read { address: address.0 })?;

        // Read back the response from the previous request.
        self.schedule_dmi_register_access(DmiOperation::NoOp)
    }

    fn read_with_timeout(
        &mut self,
        address: DmAddress,
        timeout: Duration,
    ) -> Result<u32, RiscvError> {
        // Prepare the read by sending a read request with the register address
        self.schedule_dmi_register_access(DmiOperation::Read { address: address.0 })?;

        self.dmi_register_access_with_timeout(DmiOperation::NoOp, timeout)
    }

    fn write_with_timeout(
        &mut self,
        address: DmAddress,
        value: u32,
        timeout: Duration,
    ) -> Result<Option<u32>, RiscvError> {
        self.dmi_register_access_with_timeout(
            DmiOperation::Write {
                address: address.0,
                value,
            },
            timeout,
        )
        .map(Some)
    }

    fn read_idcode(&mut self) -> Result<Option<u32>, DebugProbeError> {
        let value = self.probe.read_register(0x1, 32)?;
        Ok(Some(value.load_le::<u32>()))
    }

    fn memory_interface<'m>(
        &'m mut self,
    ) -> Result<&'m mut dyn crate::MemoryInterface<ArmError>, DebugProbeError> {
        todo!()
    }
}

fn tunnel_select_command(
    tunnel_ir_id: u32,
    tunnel_ir_width: u32,
    address: u32,
) -> JtagWriteCommand {
    let tunneled_ir: u32 = (tunnel_ir_width << (tunnel_ir_width + 3)) | (address << 3);
    let tunneled_ir_len = 1 + 7 + tunnel_ir_width + 3;
    JtagWriteCommand {
        address: tunnel_ir_id,
        data: tunneled_ir.to_le_bytes().into(),
        len: tunneled_ir_len,
        transform: |_, _| Ok(CommandResult::None),
    }
}

fn tunnel_dtmcs_command(data: u32) -> ShiftDrCommand {
    let width_offset = 1 + (DTMCS_WIDTH as u128) + 3;
    let msb_offset = 7 + width_offset;
    let tunneled_dr: u128 =
        (1 << msb_offset) | ((DTMCS_WIDTH as u128) << width_offset) | ((data as u128) << 3);
    ShiftDrCommand {
        data: tunneled_dr.to_le_bytes().into(),
        len: (msb_offset as u32) + 1,
        transform: |_, result| {
            let response = result[4..].load_le::<u32>();
            Ok(CommandResult::U32(response))
        },
    }
}

#[derive(Copy, Clone, Debug)]
pub enum DmiOperation {
    NoOp,
    Read { address: u32 },
    Write { address: u32, value: u32 },
}

impl DmiOperation {
    fn opcode(&self) -> u8 {
        match self {
            Self::NoOp => 0,
            Self::Read { .. } => 1,
            Self::Write { .. } => 2,
        }
    }

    fn register_value(&self) -> u128 {
        let (opcode, address, value): (u128, u128, u128) = match self {
            Self::NoOp => (self.opcode() as u128, 0, 0),
            Self::Read { address } => (self.opcode() as u128, *address as u128, 0),
            Self::Write { address, value } => {
                (self.opcode() as u128, *address as u128, *value as u128)
            }
        };
        (address << DMI_ADDRESS_BIT_OFFSET) | (value << DMI_VALUE_BIT_OFFSET) | opcode
    }

    pub fn to_byte_batch(self) -> [u8; 16] {
        self.register_value().to_le_bytes()
    }

    pub fn to_tunneled_byte_batch(self, dmi_bits: u32) -> (u32, [u8; 16]) {
        let width_offset = 1 + (dmi_bits) + 3;
        let msb_offset = 7 + width_offset;
        let bits = (1 << (msb_offset as u128))
            | (((dmi_bits + 1) as u128) << (width_offset as u128))
            | (self.register_value() << 3);
        (msb_offset + 1, bits.to_le_bytes())
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
            Self::Ok => Ok(()),
            Self::Reserved => unimplemented!("Reserved."),
            Self::OperationFailed => Err(RiscvError::DtmOperationFailed),
            Self::RequestInProgress => Err(RiscvError::DtmOperationInProcess),
        }
    }
}

impl DmiOperationStatus {
    pub(crate) fn parse(value: u8) -> Option<Self> {
        let status = match value {
            0 => Self::Ok,
            1 => Self::Reserved,
            2 => Self::OperationFailed,
            3 => Self::RequestInProgress,
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
