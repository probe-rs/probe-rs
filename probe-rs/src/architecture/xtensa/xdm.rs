#![allow(unused)] // FIXME remove after testing

use std::fmt::Debug;

use crate::{
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
const NARADR_DIR0EXEC: u8 = 0x47;

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

#[derive(thiserror::Error, Debug, Copy, Clone)]
pub enum DebugRegisterError {
    #[error("Busy")]
    Busy,

    #[error("Register-specific error")]
    Error,

    #[error("Unexpected value")]
    Unexpected,
}

fn parse_register_status(byte: u8) -> Result<(), DebugRegisterError> {
    match byte & 0b00000011 {
        0 => Ok(()),
        1 => Err(DebugRegisterError::Error),
        2 => Err(DebugRegisterError::Busy),
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
        // TODO calculate idle cycles? see esp32_queue_tdi_idle() in openocd
        let idle_cycles = 100;

        // Setup the number of idle cycles between JTAG accesses
        probe.set_idle_cycles(idle_cycles);

        // fixed to 5 bits for now
        probe.set_ir_len(5);

        let mut x = Self {
            probe,
            queued_commands: Vec::new(),
            device_id: 0,
            idle_cycles,
        };

        // Wakeup and enable the JTAG
        if let Err(e) = x.pwr_write(PowerDevice::PowerControl, {
            let mut control = PowerControl(0);

            control.set_debug_wakeup(true);
            control.set_mem_wakeup(true);
            control.set_core_wakeup(true);

            control.0
        }) {
            return Err((x.free(), e.into()));
        }
        if let Err(e) = x.pwr_write(PowerDevice::PowerControl, {
            let mut control = PowerControl(0);

            control.set_debug_wakeup(true);
            control.set_mem_wakeup(true);
            control.set_core_wakeup(true);
            control.set_jtag_debug_use(true);

            control.0
        }) {
            return Err((x.free(), e.into()));
        }

        // enable the debug module
        if let Err(e) = x.dbg_write(NARADR_DCRSET, 1) {
            return Err((x.free(), e.into()));
        }

        // read the device_id
        let device_id = match x.dbg_read(NARADR_OCDID) {
            Ok(value) => value,
            Err(e) => return Err((x.free(), e.into())),
        };

        if device_id == 0 || device_id == !0 {
            return Err((x.free(), DebugProbeError::TargetNotFound.into()));
        }

        let status = x.status().unwrap();
        tracing::info!("{:?}", status);
        status.is_ok().unwrap();
        // TODO check status and clear bits if required

        tracing::info!("Found Xtensa device with OCDID: 0x{:08X}", device_id);
        x.device_id = device_id;

        Ok(x)
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

        let status = self.tap_write(TapInstruction::NAR, regdata as u32);
        let res = self.tap_read(TapInstruction::NDR);

        // Check status AFTER writing NDR to avoid ending up in an incorrect state on error.
        parse_register_status(status? as u8)?;
        tracing::trace!("dbg_read response: {:?}", res);

        Ok(res?)
    }

    /// Perform an access to a register
    fn dbg_write(&mut self, address: u8, value: u32) -> Result<u32, XtensaError> {
        let regdata = (address << 1) | 1;

        let status = self.tap_write(TapInstruction::NAR, regdata as u32);
        let res = self.tap_write(TapInstruction::NDR, value);

        // Check status AFTER writing NDR to avoid ending up in an incorrect state on error.
        parse_register_status(status? as u8)?;
        tracing::trace!("dbg_write response: {:?}", res);

        Ok(res?)
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
        let bits = self.dbg_read(R::ADDRESS)?;
        R::from_bits(bits)
    }

    fn write_nexus_register<R: WritableNexusRegister>(
        &mut self,
        register: R,
    ) -> Result<(), XtensaError> {
        self.dbg_write(R::ADDRESS, register.bits())?;
        Ok(())
    }

    fn status(&mut self) -> Result<DebugStatus, XtensaError> {
        self.read_nexus_register::<DebugStatus>()
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

    pub exec_done,         _: 0;
    pub exec_exception,    _: 1;
    pub exec_busy,         _: 2;
    pub exec_overrun,      _: 3;
    pub stopped,           _: 4;
    pub core_wrote_ddr,    _: 10;
    pub core_read_ddr,     _: 11;
    pub host_wrote_ddr,    _: 14;
    pub host_read_ddr,     _: 15;
    pub debug_pend_break,  _: 16;
    pub debug_pend_host,   _: 17;
    pub debug_pend_trax,   _: 18;
    pub debug_int_break,   _: 20;
    pub debug_int_host,    _: 21;
    pub debug_int_trax,    _: 22;
    pub run_stall_toggle,  _: 23;
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

impl Debug for DebugStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DSR: {:032b}", self.0)
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
