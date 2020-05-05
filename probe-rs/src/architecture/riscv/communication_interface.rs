//! Debug Module Communication
//!
//! This module implements communication with a
//! Debug Module, as described in the RISCV debug
//! specification v0.13.2 .

use super::{register, Dmcontrol, Dmstatus};
use crate::architecture::riscv::{Abstractcs, Command, Data0, Progbuf0, Progbuf1};
use crate::DebugProbeError;
use crate::{MemoryInterface, Probe};

use crate::{CoreRegisterAddress, Error as ProbeRsError};

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
    #[error("Debug Probe Error: {0}")]
    DebugProbe(#[from] DebugProbeError),
    #[error("Timeout during JTAG register access.")]
    Timeout,
    #[error("Error occured during execution of an abstract command: {0:?}")]
    AbstractCommand(AbstractCommandErrorKind),
    #[error("The core did not acknowledge a request for reset, resume or halt")]
    RequestNotAcknowledged,
    #[error("The version '{0}' of the debug module is currently not supported.")]
    UnsupportedDebugModuleVersion(u8),
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
    initialized: bool,
    abits: u32,
}

/// Timeout for RISCV operations.
const RISCV_TIMEOUT: Duration = Duration::from_secs(5);

impl RiscvCommunicationInterfaceState {
    fn new(probe: &mut Probe) -> Result<Self, RiscvError> {
        // We need a jtag interface

        log::debug!("Building RISCV interface");

        let jtag_interface = probe
            .get_interface_jtag_mut()?
            .ok_or(DebugProbeError::InterfaceNotAvailable("JTAG"))?;

        let dtmcs_raw = jtag_interface.read_register(DTMCS_ADDRESS, DTMCS_WIDTH)?;

        let dtmcs = Dtmcs(u32::from_le_bytes((&dtmcs_raw[..]).try_into().unwrap()));

        log::debug!("Dtmcs: {:?}", dtmcs);

        let abits = dtmcs.abits();
        let idle_cycles = dtmcs.idle();

        // Setup the number of idle cycles between JTAG accesses
        jtag_interface.set_idle_cycles(idle_cycles as u8);

        let state = RiscvCommunicationInterfaceState {
            initialized: false,
            abits,
        };

        Ok(state)
    }

    pub(crate) fn initialize(&mut self) {
        self.initialized = true;
    }

    pub(crate) fn initialized(&self) -> bool {
        self.initialized
    }
}

pub struct RiscvCommunicationInterface<'probe> {
    probe: &'probe mut Probe,
    state: &'probe mut RiscvCommunicationInterfaceState,
}

impl<'probe> RiscvCommunicationInterface<'probe> {
    pub fn new(
        probe: &'probe mut Probe,
        state: &'probe mut RiscvCommunicationInterfaceState,
    ) -> Result<Option<Self>, ProbeRsError> {
        if probe.has_jtag_interface() {
            let mut s = Self { probe, state };

            if s.state.initialized() {
                s.enter_debug_mode()?;
                s.state.initialize();
            }

            Ok(Some(s))
        } else {
            log::debug!("No JTAG interface available on Probe");

            Ok(None)
        }
    }

    pub fn create_state(
        probe: &mut Probe,
    ) -> Result<RiscvCommunicationInterfaceState, ProbeRsError> {
        Ok(RiscvCommunicationInterfaceState::new(probe)?)
    }

    // TODO: N
    fn enter_debug_mode(&mut self) -> Result<(), RiscvError> {
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

        log::debug!("dmstatus: {:?}", status);

        // enable the debug module
        let mut control = Dmcontrol(0);
        control.set_dmactive(true);

        self.write_dm_register(control)
    }

    fn dmi_reset(&mut self) -> Result<(), RiscvError> {
        let mut dtmcs = Dtmcs(0);

        dtmcs.set_dmireset(true);

        let Dtmcs(reg_value) = dtmcs;

        let bytes = reg_value.to_le_bytes();

        let jtag_interface = self
            .probe
            .get_interface_jtag_mut()?
            .ok_or(DebugProbeError::InterfaceNotAvailable("JTAG"))?;

        jtag_interface.write_register(DTMCS_ADDRESS, &bytes, DTMCS_WIDTH)?;

        Ok(())
    }

    pub(crate) fn read_idcode(&mut self) -> Result<u32, DebugProbeError> {
        let jtag_interface = self
            .probe
            .get_interface_jtag_mut()?
            .ok_or(DebugProbeError::InterfaceNotAvailable("JTAG"))?;

        let value = jtag_interface.read_register(0x1, 32)?;

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

        let jtag_interface = self
            .probe
            .get_interface_jtag_mut()?
            .ok_or(DebugProbeError::InterfaceNotAvailable("JTAG"))?;

        let response_bytes = jtag_interface.write_register(DMI_ADDRESS, &bytes, bit_size)?;

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

        // Backup registers s0 and s1
        let s0 = self.abstract_cmd_register_read(&register::S0)?;

        //let o = 0; // offset = 0
        //let b = 9; // base register -> s0
        //let w = 0; // width
        //let d = 9; // dest register -> s0
        //let l = 0b11;

        let mut lw_command: u32 = 0b000000000000_01000_000_01000_0000011;

        // verify the width is supported
        // 0 ==  8 bit
        // 1 == 16 bit
        // 2 == 32 bit
        assert!((width as u32) < 3, "Width larger than 3 not supported yet");

        lw_command |= (width as u32) << 12;

        let ebreak_cmd = 0b000000000001_00000_000_00000_1110011;

        self.write_dm_register(Progbuf0(lw_command))?;
        self.write_dm_register(Progbuf1(ebreak_cmd))?;

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

        // assemble
        //  lb s0, 0(s0)

        //let o = 0; // offset = 0
        //let b = 9; // base register -> s0
        //let w = 0; // width
        //let d = 9; // dest register -> s0
        //let l = 0b11;

        //let lw_command = bitpack!("oooooooooooobbbbb_www_ddddd_lllllll");
        let mut sw_command: u32 = 0b0000000_01001_01000_000_00000_0100011;

        // sw command -> sb s1, 0(s0)

        // verify the width is supported
        // 0 ==  8 bit
        // 1 == 16 bit
        // 2 == 32 bit

        assert!((width as u32) < 3, "Width larger than 3 not supported yet");

        sw_command |= (width as u32) << 12;

        //if width == 2 {
        //    sw_command = 0xc004;
        //}

        let ebreak_cmd = 0b000000000001_00000_000_00000_1110011;

        self.write_dm_register(Progbuf0(sw_command))?;
        self.write_dm_register(Progbuf1(ebreak_cmd))?;

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
}

impl<'probe> MemoryInterface for RiscvCommunicationInterface<'probe> {
    fn read_word_32(&mut self, address: u32) -> Result<u32, crate::Error> {
        let result = self.perform_memory_read(address, RiscvBusAccess::A32)?;

        Ok(result)
    }

    fn read_word_8(&mut self, address: u32) -> Result<u8, crate::Error> {
        let value = self.perform_memory_read(address, RiscvBusAccess::A8)?;

        Ok((value & 0xff) as u8)
    }

    fn read_32(&mut self, address: u32, data: &mut [u32]) -> Result<(), crate::Error> {
        for (offset, word) in data.iter_mut().enumerate() {
            *word = self.read_word_32(address + ((offset * 4) as u32))?;
        }

        Ok(())
    }

    fn read_8(&mut self, address: u32, data: &mut [u8]) -> Result<(), crate::Error> {
        for (offset, byte) in data.iter_mut().enumerate() {
            *byte = self.read_word_8(address + (offset as u32))?;
        }

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
        for (offset, word) in data.iter().enumerate() {
            self.write_word_32(address + ((offset * 4) as u32), *word)?;
        }

        Ok(())
    }
    fn write_8(&mut self, address: u32, data: &[u8]) -> Result<(), crate::Error> {
        for (offset, byte) in data.iter().enumerate() {
            self.write_word_8(address + (offset as u32), *byte)?;
        }

        Ok(())
    }
}

/// Access width for bus access.
/// This is used both for system bus access (`sbcs` register),
/// as well for abstract commands.
#[derive(Copy, Clone, Debug)]
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
