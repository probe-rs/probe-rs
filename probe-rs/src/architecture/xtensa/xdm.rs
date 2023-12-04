#![allow(unused)] // FIXME remove after testing

use std::fmt::Debug;

use crate::{
    architecture::{arm::ap::DRW, xtensa::arch::instruction},
    probe::{JTAGAccess, JtagWriteCommand},
    DebugProbeError,
};

use super::communication_interface::XtensaError;

const NARADR_OCDID: u8 = 0x40;
const NARADR_DCRSET: u8 = 0x43;
const NARADR_DCRCLR: u8 = 0x42;
const NARADR_DSR: u8 = 0x44;
const NARADR_DDR: u8 = 0x45;
const NARADR_DDREXEC: u8 = 0x46;
// DIR0 that also executes when written
const NARADR_DIR0EXEC: u8 = 0x47;
// Assume we only support 16-24b instructions for now
const NARADR_DIR0: u8 = 0x48;

#[derive(Clone, Copy, PartialEq, Debug)]
enum TapInstruction {
    NAR,
    NDR,
    PowerControl,
    PowerStatus,
}

impl TapInstruction {
    fn code(self) -> u32 {
        match self {
            TapInstruction::NAR => 0x1C,
            TapInstruction::NDR => 0x1C,
            TapInstruction::PowerControl => 0x08,
            TapInstruction::PowerStatus => 0x09,
        }
    }

    fn bits(self) -> u32 {
        match self {
            TapInstruction::NAR => 8,
            TapInstruction::NDR => 32,
            TapInstruction::PowerControl => 8,
            TapInstruction::PowerStatus => 8,
        }
    }

    fn capture_to_u32(self, capture: &[u8]) -> u32 {
        match self {
            TapInstruction::NDR => u32::from_le_bytes(capture.try_into().unwrap()),
            _ => capture[0] as u32,
        }
    }
}

/// Power registers are separate from the other registers. They are part of the Access Port.
#[derive(Clone, Copy, PartialEq, Debug)]
enum PowerDevice {
    /// Power Control
    PowerControl,
    /// Power status
    PowerStat,
}

impl From<PowerDevice> for TapInstruction {
    fn from(dev: PowerDevice) -> Self {
        match dev {
            PowerDevice::PowerControl => TapInstruction::PowerControl,
            PowerDevice::PowerStat => TapInstruction::PowerStatus,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum DebugRegisterStatus {
    Ok,
    Busy,
    Error,
}

#[derive(thiserror::Error, Debug, Copy, Clone)]
pub enum DebugRegisterError {
    #[error("Register-specific error")]
    Error,

    #[error("Unexpected value")]
    Unexpected,
}

fn parse_register_status(byte: u8) -> Result<DebugRegisterStatus, DebugRegisterError> {
    match byte & 0b00000011 {
        0 => Ok(DebugRegisterStatus::Ok),
        1 => Ok(DebugRegisterStatus::Error),
        2 => Ok(DebugRegisterStatus::Busy),
        _ => {
            // It is not specified if both bits can be 1 at the same time.
            Err(DebugRegisterError::Unexpected)
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Error while accessing register: {0}")]
    XdmError(DebugRegisterError),

    #[error("ExecExeception")]
    ExecExeception,

    #[error("ExecBusy")]
    ExecBusy,

    #[error("ExecOverrun")]
    ExecOverrun,

    #[error("XdmPoweredOff")]
    XdmPoweredOff,
}

#[derive(Debug)]
pub struct Xdm {
    pub probe: Box<dyn JTAGAccess>,

    queued_commands: Vec<JtagWriteCommand>,

    device_id: u32,
    idle_cycles: u8,
}

impl Xdm {
    pub fn new(mut probe: Box<dyn JTAGAccess>) -> Result<Self, (Box<dyn JTAGAccess>, XtensaError)> {
        // TODO implement openocd's esp32_queue_tdi_idle() to prevent potentially damaging flash ICs

        // fixed to 5 bits for now
        probe.set_ir_len(5);

        let mut x = Self {
            probe,
            queued_commands: Vec::new(),
            device_id: 0,
            idle_cycles: 0,
        };

        if let Err(e) = x.init() {
            return Err((x.free(), e.into()));
        }

        Ok(x)
    }

    fn init(&mut self) -> Result<(), XtensaError> {
        let mut pwr_control = PowerControl(0);

        pwr_control.set_debug_wakeup(true);
        pwr_control.set_mem_wakeup(true);
        pwr_control.set_core_wakeup(true);

        // Wakeup and enable the JTAG
        self.pwr_write(PowerDevice::PowerControl, pwr_control.0)?;

        tracing::trace!("Waiting for power domain to turn on");
        loop {
            let bits = self.pwr_read(PowerDevice::PowerStat)?;
            if PowerStatus(bits).debug_domain_on() {
                break;
            }
        }

        // Set JTAG_DEBUG_USE separately to ensure it doesn't get reset by a previous write.
        // We don't reset anything but this is a good practice to avoid sneaky issues.
        pwr_control.set_jtag_debug_use(true);
        self.pwr_write(PowerDevice::PowerControl, pwr_control.0)?;

        // enable the debug module
        self.dbg_write(NARADR_DCRSET, 1)?;

        // read the device_id
        let device_id = self.dbg_read(NARADR_OCDID)?;

        if device_id == 0 || device_id == !0 {
            return Err(DebugProbeError::TargetNotFound.into());
        }

        let status = self.status()?;
        tracing::info!("{:?}", status);

        // we might find that an old instruction execution left the core with an exception
        // try to clear problematic bits
        self.write_nexus_register({
            let mut status = DebugStatus(0);

            status.set_exec_exception(true);
            status.set_exec_done(true);
            status.set_exec_overrun(true);
            status.set_debug_pend_break(true);
            status.set_debug_pend_host(true);

            status
        })?;

        // TODO check status and clear bits if required

        tracing::info!("Found Xtensa device with OCDID: 0x{:08X}", device_id);
        self.device_id = device_id;

        Ok(())
    }

    fn tap_write(&mut self, instr: TapInstruction, data: u32) -> Result<u32, DebugProbeError> {
        let capture = self
            .probe
            .write_register(instr.code(), &data.to_le_bytes(), instr.bits())?;

        Ok(instr.capture_to_u32(&capture))
    }

    fn tap_read(&mut self, instr: TapInstruction) -> Result<u32, DebugProbeError> {
        let capture = self.probe.read_register(instr.code(), instr.bits())?;

        Ok(instr.capture_to_u32(&capture))
    }

    /// Perform an access to a register
    fn dbg_read(&mut self, address: u8) -> Result<u32, XtensaError> {
        let regdata = (address << 1) | 0;

        self.tap_write(TapInstruction::NAR, regdata as u32);
        let res = self.tap_read(TapInstruction::NDR);

        tracing::trace!("dbg_read response: {:?}", res);

        Ok(res?)
    }

    /// Perform an access to a register
    fn dbg_write(&mut self, address: u8, value: u32) -> Result<u32, XtensaError> {
        let regdata = (address << 1) | 1;

        self.tap_write(TapInstruction::NAR, regdata as u32);
        let res = self.tap_write(TapInstruction::NDR, value);

        tracing::trace!("dbg_write response: {:?}", res);

        Ok(res?)
    }

    fn dbg_status(&mut self) -> Result<DebugRegisterStatus, XtensaError> {
        let status = self.tap_read(TapInstruction::NAR);
        self.tap_read(TapInstruction::NDR)?;

        Ok(parse_register_status(status? as u8)?)
    }

    fn pwr_write(&mut self, dev: PowerDevice, value: u8) -> Result<u8, XtensaError> {
        let res = self.tap_write(dev.into(), value as u32)?;
        tracing::trace!("pwr_write response: {:?}", res);

        Ok(res as u8)
    }

    fn pwr_read(&mut self, dev: PowerDevice) -> Result<u8, XtensaError> {
        let res = self.tap_read(dev.into())?;
        tracing::trace!("pwr_read response: {:?}", res);

        Ok(res as u8)
    }

    fn read_nexus_register<R: NexusRegister>(&mut self) -> Result<R, XtensaError> {
        tracing::debug!("Reading {:02x}", R::ADDRESS);
        let bits = self.dbg_read(R::ADDRESS)?;

        // TODO: this is inefficient - we should queue up the reads and then read them all at once
        self.dbg_status()?;

        tracing::debug!("Read from {:02x}: {:08x}", R::ADDRESS, bits);
        R::from_bits(bits)
    }

    fn write_nexus_register<R: WritableNexusRegister>(
        &mut self,
        register: R,
    ) -> Result<(), XtensaError> {
        tracing::debug!("Writing {:02x}", R::ADDRESS);
        self.dbg_write(R::ADDRESS, register.bits())?;

        // TODO: we should queue the first status read

        // TODO: timeout
        while self.dbg_status()? == DebugRegisterStatus::Busy {
            tracing::trace!("Waiting for write to complete");
        }

        Ok(())
    }

    fn status(&mut self) -> Result<DebugStatus, XtensaError> {
        let status = self.read_nexus_register::<DebugStatus>()?;
        tracing::debug!("Status: {:?}", status);
        Ok(status)
    }

    fn wait_for_exec_done(&mut self) -> Result<(), XtensaError> {
        // TODO add timeout
        loop {
            let status = self.status()?;

            if status.exec_overrun() {
                return Err(Error::ExecOverrun.into());
            }
            if status.exec_exception() {
                // TODO: we probably don't want to clear all clearable status bits.
                self.write_nexus_register(status);
                // TODO: we also probably don't want to crash if an exception happens here
                return Err(Error::ExecExeception.into());
            }

            if !status.exec_busy() {
                if status.exec_done() {
                    return Ok(());
                }

                tracing::warn!("Instruction ignored");
                return Ok(());
            }
        }
    }

    /// Instructs Core to enter Core Stopped state instead of vectoring on a Debug Exception/Interrupt.
    pub(super) fn enter_ocd_mode(&mut self) -> Result<(), XtensaError> {
        self.write_nexus_register(DebugControlSet({
            let mut control = DebugControlBits(0);

            control.set_enable_ocd(true);

            control
        }))?;
        self.write_nexus_register(DebugControlClear({
            let mut control = DebugControlBits(0);

            control.set_break_in_en(true);
            control.set_break_out_en(true);
            control.set_run_stall_in_en(true);
            control.set_debug_mode_out_en(true);

            control
        }))?;
        self.write_nexus_register({
            let mut status = DebugStatus(0);

            status.set_debug_pend_break(true);
            status.set_debug_int_break(true);
            status.set_exec_overrun(true);
            status.set_exec_exception(true);

            status
        });

        Ok(())
    }

    pub(super) fn is_in_ocd_mode(&mut self) -> Result<bool, XtensaError> {
        let reg = self.read_nexus_register::<DebugControlSet>()?;
        tracing::debug!("DebugControl: {:?}", reg.0);

        Ok(reg.0.enable_ocd())
    }

    pub(super) fn leave_ocd_mode(&mut self) -> Result<(), XtensaError> {
        self.resume()?;

        self.write_nexus_register(DebugControlClear({
            let mut control = DebugControlBits(0);

            control.set_enable_ocd(true);

            control
        }))?;

        Ok(())
    }

    pub(super) fn halt(&mut self) -> Result<(), XtensaError> {
        self.write_nexus_register(DebugControlSet({
            let mut control = DebugControlBits(0);

            control.set_debug_interrupt(true);

            control
        }))?;

        Ok(())
    }

    pub(super) fn resume(&mut self) -> Result<(), XtensaError> {
        // Clear pending interrupts first that would re-enter us into the Stopped state
        self.write_nexus_register({
            let mut clear_status = DebugStatus(0);

            clear_status.set_debug_pend_host(true);
            clear_status.set_debug_pend_break(true);

            clear_status
        })?;

        self.execute_instruction(instruction::rfdo(0))?;

        Ok(())
    }

    pub fn write_instruction(&mut self, instruction: u32) -> Result<(), XtensaError> {
        self.write_nexus_register(DebugInstructionRegister(instruction))
    }

    pub fn execute_instruction(&mut self, instruction: u32) -> Result<(), XtensaError> {
        self.write_nexus_register(DebugInstructionAndExecRegister(instruction))?;

        self.wait_for_exec_done()
    }

    pub fn read_ddr(&mut self) -> Result<u32, XtensaError> {
        let reg = self.read_nexus_register::<DebugDataRegister>()?;
        Ok(reg.bits())
    }

    pub fn write_ddr(&mut self, ddr: u32) -> Result<(), XtensaError> {
        self.write_nexus_register(DebugDataRegister(ddr))?;
        Ok(())
    }

    pub fn read_ddr_and_execute(&mut self) -> Result<u32, XtensaError> {
        let reg = self.read_nexus_register::<DebugDataAndExecRegister>()?;

        self.wait_for_exec_done()?;

        Ok(reg.bits())
    }

    pub fn write_ddr_and_execute(&mut self, ddr: u32) -> Result<(), XtensaError> {
        self.write_nexus_register(DebugDataAndExecRegister(ddr))?;

        self.wait_for_exec_done()?;

        Ok(())
    }

    fn free(self) -> Box<dyn JTAGAccess> {
        self.probe
    }
}

impl From<XtensaError> for crate::Error {
    fn from(err: XtensaError) -> Self {
        crate::Error::Xtensa(err)
    }
}

impl From<Error> for XtensaError {
    fn from(e: Error) -> Self {
        XtensaError::XdmError(e)
    }
}

impl From<DebugRegisterError> for Error {
    fn from(e: DebugRegisterError) -> Self {
        Error::XdmError(e)
    }
}

// TODO: I don't think these should be transformed into XtensaError directly. We might want to
// attach register-specific messages via an in-between type.
impl From<DebugRegisterError> for XtensaError {
    fn from(e: DebugRegisterError) -> Self {
        XtensaError::XdmError(e.into())
    }
}

bitfield::bitfield! {
    #[derive(Copy, Clone)]
    pub struct PowerControl(u8);

    pub core_wakeup,    set_core_wakeup:    0;
    pub mem_wakeup,     set_mem_wakeup:     1;
    pub debug_wakeup,   set_debug_wakeup:   2;
    pub core_reset,     set_core_reset:     4;
    pub debug_reset,    set_debug_reset:    6;
    pub jtag_debug_use, set_jtag_debug_use: 7;
}

bitfield::bitfield! {
    #[derive(Copy, Clone)]
    pub struct PowerStatus(u8);

    pub core_domain_on,    _: 0;
    pub mem_domain_on,     _: 1;
    pub debug_domain_on,   _: 2;
    pub core_still_needed, _: 3;
    /// Clears bit when written as 1
    pub core_was_reset,    set_core_was_reset: 4;
    /// Clears bit when written as 1
    pub debug_was_reset,   set_debug_was_reset: 6;
}

bitfield::bitfield! {
    #[derive(Copy, Clone)]
    pub struct DebugStatus(u32);
    impl Debug;

    // Cleared by writing 1
    pub exec_done,         set_exec_done: 0;
    // Cleared by writing 1
    pub exec_exception,    set_exec_exception: 1;
    pub exec_busy,         _: 2;
    // Cleared by writing 1
    pub exec_overrun,      set_exec_overrun: 3;
    pub stopped,           _: 4;
    // Cleared by writing 1
    pub core_wrote_ddr,    set_core_wrote_ddr: 10;
    // Cleared by writing 1
    pub core_read_ddr,     set_core_read_ddr: 11;
    // Cleared by writing 1
    pub host_wrote_ddr,    set_host_wrote_ddr: 14;
    // Cleared by writing 1
    pub host_read_ddr,     set_host_read_ddr: 15;
    // Cleared by writing 1
    pub debug_pend_break,  set_debug_pend_break: 16;
    // Cleared by writing 1
    pub debug_pend_host,   set_debug_pend_host: 17;
    // Cleared by writing 1
    pub debug_pend_trax,   set_debug_pend_trax: 18;
    // Cleared by writing 1
    pub debug_int_break,   set_debug_int_break: 20;
    // Cleared by writing 1
    pub debug_int_host,    set_debug_int_host: 21;
    // Cleared by writing 1
    pub debug_int_trax,    set_debug_int_trax: 22;
    // Cleared by writing 1
    pub run_stall_toggle,  set_run_stall_toggle: 23;
    pub run_stall_sample,  _: 24;
    pub break_out_ack_iti, _: 25;
    pub break_in_iti,      _: 26;
    pub dbgmod_power_on,   _: 31;
}

impl DebugStatus {
    pub fn is_ok(&self) -> Result<(), Error> {
        if self.exec_exception() {
            Err(Error::ExecExeception)
        } else if self.exec_busy() {
            Err(Error::ExecBusy)
        } else if self.exec_overrun() {
            Err(Error::ExecOverrun)
        } else if !self.dbgmod_power_on() {
            // should always be set to one
            Err(Error::XdmPoweredOff)
        } else {
            Ok(())
        }
    }
}

/// An abstraction over all registers that can be accessed via the NAR/NDR instruction pair.
trait NexusRegister: Sized + Copy {
    /// NAR register address
    const ADDRESS: u8;

    fn from_bits(bits: u32) -> Result<Self, XtensaError>;
}

trait WritableNexusRegister: NexusRegister {
    fn bits(&self) -> u32;
}

impl NexusRegister for DebugStatus {
    const ADDRESS: u8 = NARADR_DSR;

    fn from_bits(bits: u32) -> Result<Self, XtensaError> {
        Ok(Self(bits))
    }
}

impl WritableNexusRegister for DebugStatus {
    fn bits(&self) -> u32 {
        self.0
    }
}

bitfield::bitfield! {
    #[derive(Copy, Clone)]
    pub struct DebugControlBits(u32);
    impl Debug;

    pub enable_ocd,          set_enable_ocd         : 0;
    // R/set
    pub debug_interrupt,     set_debug_interrupt    : 1;
    pub interrupt_all_conds, set_interrupt_all_conds: 2;

    pub break_in_en,         set_break_in_en        : 16;
    pub break_out_en,        set_break_out_en       : 17;

    pub debug_sw_active,     set_debug_sw_active    : 20;
    pub run_stall_in_en,     set_run_stall_in_en    : 21;
    pub debug_mode_out_en,   set_debug_mode_out_en  : 22;

    pub break_out_ito,       set_break_out_ito      : 24;
    pub break_in_ack_ito,    set_break_in_ack_ito   : 25;
}

#[derive(Copy, Clone)]
/// Bits written as 1 are set to 1 in hardware.
struct DebugControlSet(DebugControlBits);

impl NexusRegister for DebugControlSet {
    const ADDRESS: u8 = NARADR_DCRSET;

    fn from_bits(bits: u32) -> Result<Self, XtensaError> {
        Ok(Self(DebugControlBits(bits)))
    }
}

impl WritableNexusRegister for DebugControlSet {
    fn bits(&self) -> u32 {
        self.0 .0
    }
}

#[derive(Copy, Clone)]
/// Bits written as 1 are set to 0 in hardware.
struct DebugControlClear(DebugControlBits);

impl NexusRegister for DebugControlClear {
    const ADDRESS: u8 = NARADR_DCRCLR;

    fn from_bits(bits: u32) -> Result<Self, XtensaError> {
        Ok(Self(DebugControlBits(bits)))
    }
}

impl WritableNexusRegister for DebugControlClear {
    fn bits(&self) -> u32 {
        self.0 .0
    }
}

/// Writes DDR.
#[derive(Copy, Clone)]
struct DebugDataRegister(u32);

impl NexusRegister for DebugDataRegister {
    const ADDRESS: u8 = NARADR_DDR;

    fn from_bits(bits: u32) -> Result<Self, XtensaError> {
        Ok(Self(bits))
    }
}

impl WritableNexusRegister for DebugDataRegister {
    fn bits(&self) -> u32 {
        self.0
    }
}

/// Writes DDR and executes DIR on write AND READ.
#[derive(Copy, Clone)]
struct DebugDataAndExecRegister(u32);

impl NexusRegister for DebugDataAndExecRegister {
    const ADDRESS: u8 = NARADR_DDREXEC;

    fn from_bits(bits: u32) -> Result<Self, XtensaError> {
        Ok(Self(bits))
    }
}

impl WritableNexusRegister for DebugDataAndExecRegister {
    fn bits(&self) -> u32 {
        self.0
    }
}

/// Writes DIR.
// TODO: type for instructions?
#[derive(Copy, Clone)]
struct DebugInstructionRegister(u32);

impl NexusRegister for DebugInstructionRegister {
    const ADDRESS: u8 = NARADR_DIR0;

    fn from_bits(bits: u32) -> Result<Self, XtensaError> {
        Ok(Self(bits))
    }
}

impl WritableNexusRegister for DebugInstructionRegister {
    fn bits(&self) -> u32 {
        self.0
    }
}

/// Writes and executes DIR.
// TODO: type for instructions?
#[derive(Copy, Clone)]
struct DebugInstructionAndExecRegister(u32);

impl NexusRegister for DebugInstructionAndExecRegister {
    const ADDRESS: u8 = NARADR_DIR0EXEC;

    fn from_bits(bits: u32) -> Result<Self, XtensaError> {
        Ok(Self(bits))
    }
}

impl WritableNexusRegister for DebugInstructionAndExecRegister {
    fn bits(&self) -> u32 {
        self.0
    }
}
