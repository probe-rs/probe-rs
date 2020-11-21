//! Debug Module Communication
//!
//! This module implements communication with a
//! Debug Module, as described in the RISCV debug
//! specification v0.13.2 .

use super::{register, Dmcontrol, Dmstatus};
use crate::architecture::riscv::*;
use crate::DebugProbeError;
use crate::{MemoryInterface, Probe};

use crate::{probe::JTAGAccess, CoreRegisterAddress, DebugProbe, Error as ProbeRsError};

use std::{
    convert::TryInto,
    time::{Duration, Instant},
};

use bitfield::bitfield;
use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum RiscvError {
    #[error("Error during read/write to the DMI register: {0:?}")]
    DmiTransfer(DmiOperationStatus),
    #[error("Debug Probe Error")]
    DebugProbe(#[from] DebugProbeError),
    #[error("Timeout during JTAG register access.")]
    Timeout,
    #[error("Error occured during execution of an abstract command: {0:?}")]
    AbstractCommand(AbstractCommandErrorKind),
    #[error("The core did not acknowledge a request for reset, resume or halt")]
    RequestNotAcknowledged,
    #[error("The version '{0}' of the debug module is currently not supported.")]
    UnsupportedDebugModuleVersion(u8),
    #[error("Program buffer register '{0}' is currently not supported.")]
    UnsupportedProgramBufferRegister(usize),
    #[error("Program buffer is too small for supplied program.")]
    ProgramBufferTooSmall,
    #[error("Memory width larger than 32 bits is not supported yet.")]
    UnsupportedBusAccessWidth(RiscvBusAccess),
    #[error("Unexpected trigger type {0} for address breakpoint.")]
    UnexpectedTriggerType(u32),
}

impl From<RiscvError> for ProbeRsError {
    fn from(err: RiscvError) -> Self {
        match err {
            RiscvError::DebugProbe(e) => e.into(),
            other => ProbeRsError::ArchitectureSpecific(Box::new(other)),
        }
    }
}

/// Errors which can occur while executing an abstract command
#[derive(Debug)]
pub(crate) enum AbstractCommandErrorKind {
    None = 0,
    Busy = 1,
    NotSupported = 2,
    Exception = 3,
    HaltResume = 4,
    Bus = 5,
    _Reserved = 6,
    Other = 7,
}

impl AbstractCommandErrorKind {
    fn parse(value: u8) -> Self {
        use AbstractCommandErrorKind::*;

        match value {
            0 => None,
            1 => Busy,
            2 => NotSupported,
            3 => Exception,
            4 => HaltResume,
            5 => Bus,
            6 => _Reserved,
            7 => Other,
            _ => panic!("cmderr is a 3 bit value, values higher than 7 should not occur."),
        }
    }
}

/// List of all debug module versions.
///
/// The version of the debug module can be read from the version field of the `dmstatus`
/// register.
#[allow(dead_code)]
#[derive(Debug, Copy, Clone, PartialEq)]
enum DebugModuleVersion {
    /// There is no debug module present.
    NoModule = 0,
    /// The debug module confirms to the version 0.11 of the RISCV Debug Specification.
    Version0_11 = 1,
    /// The debug module confirms to the version 0.13 of the RISCV Debug Specification.
    Version0_13 = 2,
    /// The debug module is present, but does not confirm to any available version of the RISCV Debug Specification.
    NonConforming = 15,
}

#[derive(Debug)]
pub struct RiscvCommunicationInterfaceState {
    abits: u32,

    /// Size of the program buffer, in 32-bit words
    progbuf_size: u8,

    /// Cache for the program buffer.
    progbuf_cache: [u32; 16],

    /// Implicit `ebreak` instruction is present after the
    /// the program buffer.
    implicit_ebreak: bool,

    /// Number of data registers for abstract commands
    data_register_count: u8,

    nscratch: u8,

    supports_autoexec: bool,
}

/// Timeout for RISCV operations.
const RISCV_TIMEOUT: Duration = Duration::from_secs(5);

impl RiscvCommunicationInterfaceState {
    pub fn new() -> Self {
        RiscvCommunicationInterfaceState {
            abits: 0,
            // Set to the minimum here, will be set to the correct value below
            progbuf_size: 0,
            progbuf_cache: [0u32; 16],

            // Assume the implicit ebreak is not present
            implicit_ebreak: false,

            // Set to the minimum here, will be set to the correct value below
            data_register_count: 1,

            nscratch: 0,

            supports_autoexec: false,
        }
    }
}

impl Default for RiscvCommunicationInterfaceState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct RiscvCommunicationInterface {
    probe: Box<dyn JTAGAccess>,
    state: RiscvCommunicationInterfaceState,
}

impl<'probe> RiscvCommunicationInterface {
    pub fn new(probe: Box<dyn JTAGAccess>) -> Result<Self, DebugProbeError> {
        let state = RiscvCommunicationInterfaceState::new();
        let mut s = Self { probe, state };

        s.enter_debug_mode()
            .map_err(|e| DebugProbeError::Other(anyhow!(e)))?;

        Ok(s)
    }

    fn enter_debug_mode(&mut self) -> Result<(), RiscvError> {
        // We need a jtag interface

        log::debug!("Building RISCV interface");

        let dtmcs_raw = self.probe.read_register(DTMCS_ADDRESS, DTMCS_WIDTH)?;

        let dtmcs = Dtmcs(u32::from_le_bytes((&dtmcs_raw[..]).try_into().unwrap()));

        log::debug!("Dtmcs: {:?}", dtmcs);

        let abits = dtmcs.abits();
        self.state.abits = abits;
        let idle_cycles = dtmcs.idle();

        // Setup the number of idle cycles between JTAG accesses
        self.probe.set_idle_cycles(idle_cycles as u8);

        // Reset error bits from previous connections
        self.dmi_reset()?;

        // read the  version of the debug module
        let status: Dmstatus = self.read_dm_register()?;

        // Only version of 0.13 of the debug specification is currently supported.
        if status.version() != DebugModuleVersion::Version0_13 as u32 {
            return Err(RiscvError::UnsupportedDebugModuleVersion(
                status.version() as u8
            ));
        }

        self.state.implicit_ebreak = status.impebreak();

        log::debug!("dmstatus: {:?}", status);

        // enable the debug module
        let mut control = Dmcontrol(0);
        control.set_dmactive(true);

        self.write_dm_register(control)?;

        // determine size of the program buffer, and number of data
        // registers for abstract commands
        let abstractcs: Abstractcs = self.read_dm_register()?;

        self.state.progbuf_size = abstractcs.progbufsize() as u8;
        log::debug!("Program buffer size: {}", self.state.progbuf_size);

        self.state.data_register_count = abstractcs.datacount() as u8;
        log::debug!(
            "Number of data registers: {}",
            self.state.data_register_count
        );

        // determine more information about hart
        let hartinfo: Hartinfo = self.read_dm_register()?;

        self.state.nscratch = hartinfo.nscratch() as u8;
        log::debug!("Number of dscratch registers: {}", self.state.nscratch);

        // determine if autoexec works
        let mut abstractauto = Abstractauto(0);
        abstractauto.set_autoexecprogbuf(2u32.pow(self.state.progbuf_size as u32) - 1);
        abstractauto.set_autoexecdata(2u32.pow(self.state.data_register_count as u32) - 1);

        self.write_dm_register(abstractauto)?;

        let abstractauto_readback: Abstractauto = self.read_dm_register()?;

        self.state.supports_autoexec = abstractauto_readback == abstractauto;
        log::debug!("Support for autoexec: {}", self.state.supports_autoexec);

        Ok(())
    }

    fn dmi_reset(&mut self) -> Result<(), RiscvError> {
        let mut dtmcs = Dtmcs(0);

        dtmcs.set_dmireset(true);

        let Dtmcs(reg_value) = dtmcs;

        let bytes = reg_value.to_le_bytes();

        self.probe
            .write_register(DTMCS_ADDRESS, &bytes, DTMCS_WIDTH)?;

        Ok(())
    }

    pub(crate) fn read_idcode(&mut self) -> Result<u32, DebugProbeError> {
        let value = self.probe.read_register(0x1, 32)?;

        Ok(u32::from_le_bytes((&value[..]).try_into().unwrap()))
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
    ) -> Result<u32, RiscvError> {
        let register_value: u128 = ((address as u128) << DMI_ADDRESS_BIT_OFFSET)
            | ((value as u128) << DMI_VALUE_BIT_OFFSET)
            | op as u128;

        let bytes = register_value.to_le_bytes();

        let bit_size = self.state.abits + DMI_ADDRESS_BIT_OFFSET;

        let response_bytes = self.probe.write_register(DMI_ADDRESS, &bytes, bit_size)?;

        let response_value: u128 = response_bytes.iter().enumerate().fold(0, |acc, elem| {
            let (byte_offset, value) = elem;
            acc + ((*value as u128) << (8 * byte_offset))
        });

        // Verify that the transfer was ok
        let op = (response_value & DMI_OP_MASK) as u8;

        if op != 0 {
            return Err(RiscvError::DmiTransfer(
                DmiOperationStatus::parse(op).unwrap(),
            ));
        }

        let value = (response_value >> 2) as u32;

        Ok(value)
    }

    /// Read or write the `dmi` register. If a busy value is rerurned, the access is
    /// retried until the transfer either succeeds, or the tiemout expires.
    fn dmi_register_access_with_timeout(
        &mut self,
        address: u64,
        value: u32,
        op: DmiOperation,
        timeout: Duration,
    ) -> Result<u32, RiscvError> {
        let start_time = Instant::now();

        loop {
            match self.dmi_register_access(address, value, op) {
                Ok(result) => return Ok(result),
                Err(RiscvError::DmiTransfer(DmiOperationStatus::RequestInProgress)) => {
                    // Operation still in progress, reset dmi status and try again.
                    self.dmi_reset()?;
                }
                Err(e) => return Err(e),
            }

            if start_time.elapsed() > timeout {
                return Err(RiscvError::Timeout);
            }
        }
    }

    pub(super) fn read_dm_register<R: DebugRegister>(&mut self) -> Result<R, RiscvError> {
        log::debug!("Reading DM register '{}' at {:#010x}", R::NAME, R::ADDRESS);

        // Prepare the read by sending a read request with the register address
        self.dmi_register_access_with_timeout(
            R::ADDRESS as u64,
            0,
            DmiOperation::Read,
            RISCV_TIMEOUT,
        )?;

        // Read back the response from the previous request.
        let response =
            self.dmi_register_access_with_timeout(0, 0, DmiOperation::NoOp, RISCV_TIMEOUT)?;

        log::debug!(
            "Read DM register '{}' at {:#010x} = {:#010x}",
            R::NAME,
            R::ADDRESS,
            response
        );

        Ok(response.into())
    }

    pub(super) fn write_dm_register<R: DebugRegister>(
        &mut self,
        register: R,
    ) -> Result<(), RiscvError> {
        // write write command to dmi register

        let data = register.into();

        log::debug!(
            "Write DM register '{}' at {:#010x} = {:#010x}",
            R::NAME,
            R::ADDRESS,
            data
        );

        self.dmi_register_access_with_timeout(
            R::ADDRESS as u64,
            data,
            DmiOperation::Write,
            RISCV_TIMEOUT,
        )?;

        Ok(())
    }

    fn write_progbuf(&mut self, index: usize, value: u32) -> Result<(), RiscvError> {
        match index {
            0 => self.write_dm_register(Progbuf0(value)),
            1 => self.write_dm_register(Progbuf1(value)),
            2 => self.write_dm_register(Progbuf2(value)),
            3 => self.write_dm_register(Progbuf3(value)),
            4 => self.write_dm_register(Progbuf4(value)),
            5 => self.write_dm_register(Progbuf5(value)),
            6 => self.write_dm_register(Progbuf6(value)),
            7 => self.write_dm_register(Progbuf7(value)),
            8 => self.write_dm_register(Progbuf8(value)),
            9 => self.write_dm_register(Progbuf9(value)),
            10 => self.write_dm_register(Progbuf10(value)),
            11 => self.write_dm_register(Progbuf11(value)),
            12 => self.write_dm_register(Progbuf12(value)),
            13 => self.write_dm_register(Progbuf13(value)),
            14 => self.write_dm_register(Progbuf14(value)),
            15 => self.write_dm_register(Progbuf15(value)),
            e => Err(RiscvError::UnsupportedProgramBufferRegister(e)),
        }
    }

    pub(crate) fn setup_program_buffer(&mut self, data: &[u32]) -> Result<(), RiscvError> {
        let required_len = if self.state.implicit_ebreak {
            data.len()
        } else {
            data.len() + 1
        };

        if required_len > self.state.progbuf_size as usize {
            return Err(RiscvError::ProgramBufferTooSmall);
        }

        if data == &self.state.progbuf_cache[..data.len()] {
            // Check if we actually have to write the program buffer
            log::debug!("Program buffer is up-to-date, skipping write.");
            return Ok(());
        }

        for (index, word) in data.iter().enumerate() {
            self.write_progbuf(index, *word)?;
        }

        // Add manual ebreak if necessary.
        //
        // This is necessary when we either don't need the full program buffer,
        // or if there is no implict ebreak after the last program buffer word.
        if !self.state.implicit_ebreak || data.len() < self.state.progbuf_size as usize {
            self.write_progbuf(data.len(), assembly::EBREAK)?;
        }

        // Update the cache
        self.state.progbuf_cache[..data.len()].copy_from_slice(data);

        Ok(())
    }

    /// Perfrom memory read from a single location using the program buffer.
    /// Only reads up to a width of 32 bits are currently supported.
    /// For widths smaller than u32, the higher bits have to be discarded manually.
    fn perform_memory_read(
        &mut self,
        address: u32,
        width: RiscvBusAccess,
    ) -> Result<u32, RiscvError> {
        // assemble
        //  lb s1, 0(s0)

        // Backup register s0
        let s0 = self.abstract_cmd_register_read(&register::S0)?;

        if width > RiscvBusAccess::A32 {
            return Err(RiscvError::UnsupportedBusAccessWidth(width));
        }

        let lw_command: u32 = assembly::lw(0, 8, width as u32, 8);

        self.setup_program_buffer(&[lw_command])?;

        self.write_dm_register(Data0(address))?;

        // Write s0, then execute program buffer
        let mut command = AccessRegisterCommand(0);
        command.set_cmd_type(0);
        command.set_transfer(true);
        command.set_write(true);

        // registers are 32 bit, so we have size 2 here
        command.set_aarsize(RiscvBusAccess::A32);
        command.set_postexec(true);

        // register s0, ie. 0x1008
        command.set_regno((register::S0).address.0 as u32);

        self.write_dm_register(command)?;

        let status: Abstractcs = self.read_dm_register()?;

        if status.cmderr() != 0 {
            return Err(RiscvError::AbstractCommand(
                AbstractCommandErrorKind::parse(status.cmderr() as u8),
            ));
        }

        // Read back s0
        let value = self.abstract_cmd_register_read(&register::S0)?;

        // Restore s0 register
        self.abstract_cmd_register_write(&register::S0, s0)?;

        Ok(value)
    }

    /// Perform memory write to a single location using the program buffer.
    /// Only writes up to a width of 32 bits are currently supported.
    fn perform_memory_write(
        &mut self,
        address: u32,
        width: RiscvBusAccess,
        data: u32,
    ) -> Result<(), RiscvError> {
        // Backup registers s0 and s1
        let s0 = self.abstract_cmd_register_read(&register::S0)?;
        let s1 = self.abstract_cmd_register_read(&register::S1)?;

        if width > RiscvBusAccess::A32 {
            return Err(RiscvError::UnsupportedBusAccessWidth(width));
        }

        let sw_command = assembly::sw(0, 8, width as u32, 9);

        self.setup_program_buffer(&[sw_command])?;

        // write value into s0
        self.abstract_cmd_register_write(&register::S0, address)?;

        // write address into data 0
        self.write_dm_register(Data0(data))?;

        // Write s0, then execute program buffer
        let mut command = AccessRegisterCommand(0);
        command.set_cmd_type(0);
        command.set_transfer(true);
        command.set_write(true);

        // registers are 32 bit, so we have size 2 here
        command.set_aarsize(RiscvBusAccess::A32);
        command.set_postexec(true);

        // register s0, ie. 0x1008
        command.set_regno((register::S1).address.0 as u32);

        self.write_dm_register(command)?;

        let status: Abstractcs = self.read_dm_register()?;

        if status.cmderr() != 0 {
            return Err(RiscvError::AbstractCommand(
                AbstractCommandErrorKind::parse(status.cmderr() as u8),
            ));
        }

        // Restore register s0 and s1

        self.abstract_cmd_register_write(&register::S0, s0)?;
        self.abstract_cmd_register_write(&register::S1, s1)?;

        Ok(())
    }

    pub(crate) fn execute_abstract_command(&mut self, command: u32) -> Result<(), RiscvError> {
        // ensure that preconditions are fullfileld
        // haltreq      = 0
        // resumereq    = 0
        // ackhavereset = 0

        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_haltreq(false);
        dmcontrol.set_resumereq(false);
        dmcontrol.set_ackhavereset(true);
        dmcontrol.set_dmactive(true);
        self.write_dm_register(dmcontrol)?;

        // read abstractcs to see its state
        let abstractcs_prev: Abstractcs = self.read_dm_register()?;

        log::debug!("abstractcs: {:?}", abstractcs_prev);

        if abstractcs_prev.cmderr() != 0 {
            //clear previous command error
            let mut abstractcs_clear = Abstractcs(0);
            abstractcs_clear.set_cmderr(0x7);

            self.write_dm_register(abstractcs_clear)?;
        }

        self.write_dm_register(Command(command))?;

        // poll busy flag in abstractcs

        let start_time = Instant::now();

        let mut abstractcs: Abstractcs;

        loop {
            abstractcs = self.read_dm_register()?;

            if !abstractcs.busy() {
                break;
            }

            if start_time.elapsed() > RISCV_TIMEOUT {
                return Err(RiscvError::Timeout);
            }
        }

        log::debug!("abstracts: {:?}", abstractcs);

        // check cmderr
        if abstractcs.cmderr() != 0 {
            return Err(RiscvError::AbstractCommand(
                AbstractCommandErrorKind::parse(abstractcs.cmderr() as u8),
            ));
        }

        Ok(())
    }

    // Read a core register using an abstract command
    pub(crate) fn abstract_cmd_register_read(
        &mut self,
        regno: impl Into<CoreRegisterAddress>,
    ) -> Result<u32, RiscvError> {
        // GPR

        // read from data0
        let mut command = AccessRegisterCommand(0);
        command.set_cmd_type(0);
        command.set_transfer(true);
        command.set_aarsize(RiscvBusAccess::A32);

        command.set_regno(regno.into().0 as u32);

        self.execute_abstract_command(command.0)?;

        let register_value: Data0 = self.read_dm_register()?;

        Ok(register_value.into())
    }

    pub(crate) fn abstract_cmd_register_write(
        &mut self,
        regno: impl Into<CoreRegisterAddress>,
        value: u32,
    ) -> Result<(), RiscvError> {
        // write to data0
        let mut command = AccessRegisterCommand(0);
        command.set_cmd_type(0);
        command.set_transfer(true);
        command.set_write(true);
        command.set_aarsize(RiscvBusAccess::A32);

        command.set_regno(regno.into().0 as u32);

        // write data0
        self.write_dm_register(Data0(value))?;

        self.execute_abstract_command(command.0)?;

        Ok(())
    }

    pub fn close(self) -> Probe {
        Probe::from_attached_probe(self.probe.into_probe())
    }
}

impl<'a> AsRef<dyn DebugProbe + 'a> for RiscvCommunicationInterface {
    fn as_ref(&self) -> &(dyn DebugProbe + 'a) {
        self.probe.as_ref().as_ref()
    }
}

impl<'a> AsMut<dyn DebugProbe + 'a> for RiscvCommunicationInterface {
    fn as_mut(&mut self) -> &mut (dyn DebugProbe + 'a) {
        self.probe.as_mut().as_mut()
    }
}

impl MemoryInterface for RiscvCommunicationInterface {
    fn read_word_32(&mut self, address: u32) -> Result<u32, crate::Error> {
        let result = self.perform_memory_read(address, RiscvBusAccess::A32)?;

        Ok(result)
    }

    fn read_word_8(&mut self, address: u32) -> Result<u8, crate::Error> {
        let value = self.perform_memory_read(address, RiscvBusAccess::A8)?;

        Ok((value & 0xff) as u8)
    }

    fn read_32(&mut self, address: u32, data: &mut [u32]) -> Result<(), crate::Error> {
        log::debug!("read_32 from {:#08x}", address);
        //  lb s1, 0(s0)

        // Backup registers s0 and s1
        let s0 = self.abstract_cmd_register_read(&register::S0)?;
        let s1 = self.abstract_cmd_register_read(&register::S1)?;

        let lw_command: u32 = assembly::lw(0, 8, RiscvBusAccess::A32 as u32, 9);

        self.setup_program_buffer(&[lw_command, assembly::addi(8, 8, 4)])?;

        self.write_dm_register(Data0(address))?;

        // Write s0, then execute program buffer
        let mut command = AccessRegisterCommand(0);
        command.set_cmd_type(0);
        command.set_transfer(true);
        command.set_write(true);

        // registers are 32 bit, so we have size 2 here
        command.set_aarsize(RiscvBusAccess::A32);
        command.set_postexec(true);

        // register s0, ie. 0x1008
        command.set_regno((register::S0).address.0 as u32);

        self.write_dm_register(command)?;

        let data_len = data.len();

        for word in &mut data[..data_len - 1] {
            let mut command = AccessRegisterCommand(0);
            command.set_cmd_type(0);
            command.set_transfer(true);
            command.set_write(false);

            // registers are 32 bit, so we have size 2 here
            command.set_aarsize(RiscvBusAccess::A32);
            command.set_postexec(true);

            command.set_regno((register::S1).address.0 as u32);

            self.write_dm_register(command)?;

            // Read back s1
            let value: Data0 = self.read_dm_register()?;

            *word = value.0;
        }

        let last_value = self.abstract_cmd_register_read(&register::S1)?;

        data[data.len() - 1] = last_value;

        let status: Abstractcs = self.read_dm_register()?;

        if status.cmderr() != 0 {
            return Err(RiscvError::AbstractCommand(AbstractCommandErrorKind::parse(
                status.cmderr() as u8,
            ))
            .into());
        }

        // Restore s0 register
        self.abstract_cmd_register_write(&register::S0, s0)?;
        self.abstract_cmd_register_write(&register::S1, s1)?;

        Ok(())
    }

    /// Read 8-bit values from target memory.
    fn read_8(&mut self, address: u32, data: &mut [u8]) -> Result<(), crate::Error> {
        log::debug!("read_8 from {:#08x}", address);

        // Backup registers s0 and s1
        let s0 = self.abstract_cmd_register_read(&register::S0)?;
        let s1 = self.abstract_cmd_register_read(&register::S1)?;

        let lw_command: u32 = assembly::lw(0, 8, RiscvBusAccess::A8 as u32, 9);

        self.setup_program_buffer(&[lw_command, assembly::addi(8, 8, 1)])?;

        self.write_dm_register(Data0(address))?;

        // Write s0, then execute program buffer
        let mut command = AccessRegisterCommand(0);
        command.set_cmd_type(0);
        command.set_transfer(true);
        command.set_write(true);

        // registers are 32 bit, so we have size 2 here
        command.set_aarsize(RiscvBusAccess::A32);
        command.set_postexec(true);

        // register s0, ie. 0x1008
        command.set_regno((register::S0).address.0 as u32);

        self.write_dm_register(command)?;

        let data_len = data.len();

        for word in &mut data[..data_len - 1] {
            let mut command = AccessRegisterCommand(0);
            command.set_cmd_type(0);
            command.set_transfer(true);
            command.set_write(false);

            // registers are 32 bit, so we have size 2 here
            command.set_aarsize(RiscvBusAccess::A32);
            command.set_postexec(true);

            command.set_regno((register::S1).address.0 as u32);

            self.write_dm_register(command)?;

            // Read back s1
            let value: Data0 = self.read_dm_register()?;

            *word = value.0 as u8;
        }

        let last_value = self.abstract_cmd_register_read(&register::S1)?;

        data[data.len() - 1] = last_value as u8;

        let status: Abstractcs = self.read_dm_register()?;

        if status.cmderr() != 0 {
            return Err(RiscvError::AbstractCommand(AbstractCommandErrorKind::parse(
                status.cmderr() as u8,
            ))
            .into());
        }

        // Restore s0 register
        self.abstract_cmd_register_write(&register::S0, s0)?;
        self.abstract_cmd_register_write(&register::S1, s1)?;

        Ok(())
    }

    fn write_word_32(&mut self, address: u32, data: u32) -> Result<(), crate::Error> {
        self.perform_memory_write(address, RiscvBusAccess::A32, data)?;

        Ok(())
    }

    fn write_word_8(&mut self, address: u32, data: u8) -> Result<(), crate::Error> {
        self.perform_memory_write(address, RiscvBusAccess::A8, data as u32)?;

        Ok(())
    }

    fn write_32(&mut self, address: u32, data: &[u32]) -> Result<(), crate::Error> {
        log::debug!("write_32 to {:#08x}", address);

        let s0 = self.abstract_cmd_register_read(&register::S0)?;
        let s1 = self.abstract_cmd_register_read(&register::S1)?;

        // Setup program buffer for multiple writes
        // Store value from register s0 into memory,
        // then increase the address for next write.
        let sw_command = assembly::sw(0, 8, RiscvBusAccess::A32 as u32, 9);

        self.setup_program_buffer(&[sw_command, assembly::addi(8, 8, 4)])?;

        // write address into s0
        self.abstract_cmd_register_write(&register::S0, address)?;

        for value in data {
            // write address into data 0
            self.write_dm_register(Data0(*value as u32))?;

            // Write s0, then execute program buffer
            let mut command = AccessRegisterCommand(0);
            command.set_cmd_type(0);
            command.set_transfer(true);
            command.set_write(true);

            // registers are 32 bit, so we have size 2 here
            command.set_aarsize(RiscvBusAccess::A32);
            command.set_postexec(true);

            // register s1
            command.set_regno((register::S1).address.0 as u32);

            self.write_dm_register(command)?;
        }

        // Errors are sticky, so we can just check at the end if everything worked.
        let status: Abstractcs = self.read_dm_register()?;

        if status.cmderr() != 0 {
            return Err(DebugProbeError::ArchitectureSpecific(Box::new(
                RiscvError::AbstractCommand(AbstractCommandErrorKind::parse(status.cmderr() as u8)),
            ))
            .into());
        }

        // Restore register s0 and s1

        self.abstract_cmd_register_write(&register::S0, s0)?;
        self.abstract_cmd_register_write(&register::S1, s1)?;

        Ok(())
    }

    fn write_8(&mut self, address: u32, data: &[u8]) -> Result<(), crate::Error> {
        log::debug!("write_8 to {:#08x}", address);

        //fn perform_memory_write(
        //    &mut self,
        //    address: u32,
        //    width: RiscvBusAccess,
        //    data: u32,
        //) -> Result<(), RiscvError> {
        // Backup registers s0 and s1
        let s0 = self.abstract_cmd_register_read(&register::S0)?;
        let s1 = self.abstract_cmd_register_read(&register::S1)?;

        let sw_command = assembly::sw(0, 8, RiscvBusAccess::A8 as u32, 9);

        self.setup_program_buffer(&[sw_command, assembly::addi(8, 8, 1)])?;

        // write value into s0
        self.abstract_cmd_register_write(&register::S0, address)?;

        for value in data {
            // write address into data 0
            self.write_dm_register(Data0(*value as u32))?;

            // Write s0, then execute program buffer
            let mut command = AccessRegisterCommand(0);
            command.set_cmd_type(0);
            command.set_transfer(true);
            command.set_write(true);

            // registers are 32 bit, so we have size 2 here
            command.set_aarsize(RiscvBusAccess::A32);
            command.set_postexec(true);

            // register s0, ie. 0x1008
            command.set_regno((register::S1).address.0 as u32);

            self.write_dm_register(command)?;
        }

        let status: Abstractcs = self.read_dm_register()?;

        if status.cmderr() != 0 {
            return Err(DebugProbeError::ArchitectureSpecific(Box::new(
                RiscvError::AbstractCommand(AbstractCommandErrorKind::parse(status.cmderr() as u8)),
            ))
            .into());
        }

        // Restore register s0 and s1

        self.abstract_cmd_register_write(&register::S0, s0)?;
        self.abstract_cmd_register_write(&register::S1, s1)?;

        Ok(())
    }

    fn flush(&mut self) -> Result<(), crate::Error> {
        Ok(())
    }
}

/// Access width for bus access.
/// This is used both for system bus access (`sbcs` register),
/// as well for abstract commands.
#[derive(Copy, Clone, PartialEq, PartialOrd, Debug)]
pub enum RiscvBusAccess {
    A8 = 0,
    A16 = 1,
    A32 = 2,
    A64 = 3,
    A128 = 4,
}

impl From<RiscvBusAccess> for u8 {
    fn from(value: RiscvBusAccess) -> Self {
        value as u8
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

bitfield! {
    /// Abstract command register, located at address 0x17
    /// This is not for all commands, only for the ones
    /// from the debug spec.
    pub struct AccessRegisterCommand(u32);
    impl Debug;
    pub _, set_cmd_type: 31, 24;
    pub u8, from into RiscvBusAccess, _, set_aarsize: 22, 20;
    pub _, set_aarpostincrement: 19;
    pub _, set_postexec: 18;
    pub _, set_transfer: 17;
    pub _, set_write: 16;
    pub _, set_regno: 15, 0;
}

impl DebugRegister for AccessRegisterCommand {
    const ADDRESS: u8 = 0x17;
    const NAME: &'static str = "command";
}

impl From<AccessRegisterCommand> for u32 {
    fn from(register: AccessRegisterCommand) -> Self {
        register.0
    }
}

impl From<u32> for AccessRegisterCommand {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

pub(super) trait DebugRegister: Into<u32> + From<u32> {
    const ADDRESS: u8;
    const NAME: &'static str;
}

bitfield! {
    pub struct Sbcs(u32);
    impl Debug;

    sbversion, _: 31, 29;
    sbbusyerror, set_sbbusyerror: 22;
    sbbusy, _: 21;
    sbreadonaddr, set_sbreadonaddr: 20;
    sbaccess, set_sbaccess: 19, 17;
    sbautoincrement, set_sbautoincrement: 16;
    sbreadondata, set_sbreadondata: 16;
    sberror, set_sberror: 14, 12;
    sbasize, _: 11, 5;
    sbaccess128, _: 4;
    sbaccess64, _: 3;
    sbaccess32, _: 2;
    sbaccess16, _: 1;
    sbaccess8, _: 0;
}

impl DebugRegister for Sbcs {
    const ADDRESS: u8 = 0x38;
    const NAME: &'static str = "sbcs";
}

impl From<Sbcs> for u32 {
    fn from(register: Sbcs) -> Self {
        register.0
    }
}

bitfield! {
    #[derive(Copy, Clone, PartialEq)]
    pub struct Abstractauto(u32);
    impl Debug;

    autoexecprogbuf, set_autoexecprogbuf: 31, 16;
    autoexecdata, set_autoexecdata: 11, 0;
}

impl DebugRegister for Abstractauto {
    const ADDRESS: u8 = 0x38;
    const NAME: &'static str = "abstractauto";
}

impl From<Abstractauto> for u32 {
    fn from(register: Abstractauto) -> Self {
        register.0
    }
}

impl From<u32> for Abstractauto {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<u32> for Sbcs {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

bitfield! {
    /// Abstract command register, located at address 0x17
    /// This is not for all commands, only for the ones
    /// from the debug spec.
    pub struct AccessMemoryCommand(u32);
    impl Debug;
    _, set_cmd_type: 31, 24;
    pub _, set_aamvirtual: 23;
    pub _, set_aamsize: 22,20;
    pub _, set_aampostincrement: 19;
    pub _, set_write: 16;
    pub _, set_target_specific: 15, 14;
}

impl DebugRegister for AccessMemoryCommand {
    const ADDRESS: u8 = 0x17;
    const NAME: &'static str = "command";
}

impl From<AccessMemoryCommand> for u32 {
    fn from(register: AccessMemoryCommand) -> Self {
        let mut reg = register;
        reg.set_cmd_type(2);
        reg.0
    }
}

impl From<u32> for AccessMemoryCommand {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

data_register! { Sbaddress0, 0x39, "sbaddress0" }
data_register! { Sbaddress1, 0x3a, "sbaddress1" }
data_register! { Sbaddress2, 0x3b, "sbaddress2" }
data_register! { Sbaddress3, 0x37, "sbaddress3" }

data_register! { Sbdata0, 0x3c, "sbdata0" }
data_register! { Sbdata1, 0x3d, "sbdata1" }
data_register! { Sbdata2, 0x3e, "sbdata2" }
data_register! { Sbdata3, 0x3f, "sbdata3" }

/// Possible return values in the op field of
/// the dmi register.
#[derive(Debug)]
pub(crate) enum DmiOperationStatus {
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

#[derive(Copy, Clone, Debug)]
enum DmiOperation {
    NoOp = 0,
    Read = 1,
    Write = 2,
    _Reserved = 3,
}
