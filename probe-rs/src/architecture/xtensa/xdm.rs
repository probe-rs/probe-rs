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

const XDM_REGISTER_WIDTH: u32 = 32;
const XDM_ADDRESS_REGISTER_WIDTH: u32 = 8;

const PWRCTL_JTAGDEBUGUSE: u8 = 1 << 7;
const PWRCTL_DEBUGRESET: u8 = 1 << 6;
const PWRCTL_CORERESET: u8 = 1 << 4;
const PWRCTL_DEBUGWAKEUP: u8 = 1 << 2;
const PWRCTL_MEMWAKEUP: u8 = 1 << 1;
const PWRCTL_COREWAKEUP: u8 = 1 << 0;

const PWRSTAT_DEBUGWASRESET: u8 = 1 << 6;
const PWRSTAT_COREWASRESET: u8 = 1 << 4;
const PWRSTAT_CORESTILLNEEDED: u8 = 1 << 3;
const PWRSTAT_DEBUGDOMAINON: u8 = 1 << 2;
const PWRSTAT_MEMDOMAINON: u8 = 1 << 1;
const PWRSTAT_COREDOMAINON: u8 = 1 << 0;

// The debug module is accesible through NARSEL JTAG register (NAR for IR, NDR for DR)
const DEBUG_ADDR: u32 = 0x1C;

#[repr(u32)]
enum PowerDevice {
    /// Power Control
    PowerControl = 0x08,
    /// Power status
    PowerStat = 0x09,
}

#[derive(thiserror::Error, Debug, Copy, Clone)]
pub enum XdmStatus {
    Ok,
    Busy,
    Error,
}

impl XdmStatus {
    pub fn is_ok(&self) -> Result<(), Error> {
        match self {
            XdmStatus::Ok => Ok(()),
            other => Err(Error::XdmError(Some(*self)))
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    XdmError(Option<XdmStatus>),
    ExecExeception,
    ExecBusy,
    ExecOverrun,
    XdmPoweredOff
}

impl XdmStatus {
    fn parse(byte: u8) -> Result<XdmStatus, XtensaError> {
        let byte = byte & 0b00000011;
        Ok(match byte {
            0 => XdmStatus::Ok,
            1 => XdmStatus::Error,
            2 => XdmStatus::Busy,
            _ => return Err(XtensaError::XdmError(Error::XdmError(None))),
        })
    }
}

impl core::fmt::Display for XdmStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{:?}", self)
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{:?}", self)
    }
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
        if let Err(e) = x.pwr_write(
            PowerDevice::PowerControl,
            PWRCTL_DEBUGWAKEUP | PWRCTL_MEMWAKEUP | PWRCTL_COREWAKEUP,
        ) {
            return Err((x.free(), e.into()));
        }
        if let Err(e) = x.pwr_write(
            PowerDevice::PowerControl,
            PWRCTL_DEBUGWAKEUP | PWRCTL_MEMWAKEUP | PWRCTL_COREWAKEUP | PWRCTL_JTAGDEBUGUSE,
        ) {
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
        log::info!("DSR: {:?}", status);
        status.is_ok().unwrap();
        // TODO check status and clear bits if required

        log::info!("Found Xtensa device with OCDID: 0x{:08X}", device_id);
        x.device_id = device_id;

        Ok(x)
    }

    /// Perform an access to a register
    fn dbg_read(&mut self, address: u8) -> Result<u32, XtensaError> {
        let regdata = (address << 1) | 0;

        XdmStatus::parse(
            self.probe
                .write_register(DEBUG_ADDR, &[regdata], XDM_ADDRESS_REGISTER_WIDTH)?[0],
        )?.is_ok()?;

        let res = self.probe.read_register(DEBUG_ADDR, XDM_REGISTER_WIDTH)?;
        log::trace!("dbg_read response: {:?}", res);

        Ok(u32::from_le_bytes((&res[..]).try_into().unwrap()))
    }

    /// Perform an access to a register
    fn dbg_write(&mut self, address: u8, value: u32) -> Result<u32, XtensaError> {
        let regdata = (address << 1) | 1;

        XdmStatus::parse(
            self.probe
                .write_register(DEBUG_ADDR, &[regdata], XDM_ADDRESS_REGISTER_WIDTH)?[0],
        )?.is_ok()?;

        let res =
            self.probe
                .write_register(DEBUG_ADDR, &value.to_le_bytes()[..], XDM_REGISTER_WIDTH)?;

        log::trace!("dbg_write response: {:?}", res);

        Ok(u32::from_le_bytes((&res[..]).try_into().unwrap()))
    }

    fn pwr_write(&mut self, dev: PowerDevice, value: u8) -> Result<XdmStatus, XtensaError> {
        let res = XdmStatus::parse(
            self.probe
                .write_register(dev as u32, &[value], XDM_ADDRESS_REGISTER_WIDTH)?[0],
        )?;
        log::trace!("pwr_write response: {:?}", res);

        Ok(res)
    }

    fn pwr_read(&mut self, dev: PowerDevice) -> Result<XdmStatus, XtensaError> {
        let res = XdmStatus::parse(
            self.probe
                .read_register(dev as u32, XDM_ADDRESS_REGISTER_WIDTH)?[0],
        )?;
        log::trace!("pwr_read response: {:?}", res);

        Ok(res)
    }

    fn status(&mut self) -> Result<DebugStatus, XtensaError> {
        Ok(DebugStatus::new(self.dbg_read(NARADR_DSR)?))
    }

    fn free(self) -> Box<dyn JTAGAccess> {
        self.probe
    }
}

impl From<XtensaError> for crate::Error {
    fn from(err: XtensaError) -> Self {
        match err {
            XtensaError::DebugProbe(e) => e.into(),
            other => crate::Error::ArchitectureSpecific(Box::new(other)),
        }
    }
}

impl From<Error> for XtensaError {
    fn from(e: Error) -> Self {
        XtensaError::XdmError(e)
    }
}

#[derive(Copy, Clone)]
pub struct DebugStatus(u32);

impl DebugStatus {
    pub const OCDDSR_EXECDONE: u32 = (1 << 0);
    pub const OCDDSR_EXECEXCEPTION: u32 = (1 << 1);
    pub const OCDDSR_EXECBUSY: u32 = (1 << 2);
    pub const OCDDSR_EXECOVERRUN: u32 = (1 << 3);
    pub const OCDDSR_STOPPED: u32 = (1 << 4);
    pub const OCDDSR_COREWROTEDDR: u32 = (1 << 10);
    pub const OCDDSR_COREREADDDR: u32 = (1 << 11);
    pub const OCDDSR_HOSTWROTEDDR: u32 = (1 << 14);
    pub const OCDDSR_HOSTREADDDR: u32 = (1 << 15);
    pub const OCDDSR_DEBUGPENDBREAK: u32 = (1 << 16);
    pub const OCDDSR_DEBUGPENDHOST: u32 = (1 << 17);
    pub const OCDDSR_DEBUGPENDTRAX: u32 = (1 << 18);
    pub const OCDDSR_DEBUGINTBREAK: u32 = (1 << 20);
    pub const OCDDSR_DEBUGINTHOST: u32 = (1 << 21);
    pub const OCDDSR_DEBUGINTTRAX: u32 = (1 << 22);
    pub const OCDDSR_RUNSTALLTOGGLE: u32 = (1 << 23);
    pub const OCDDSR_RUNSTALLSAMPLE: u32 = (1 << 24);
    pub const OCDDSR_BREACKOUTACKITI: u32 = (1 << 25);
    pub const OCDDSR_BREAKINITI: u32 = (1 << 26);
    pub const OCDDSR_DBGMODPOWERON: u32 = (1 << 31);

    pub fn new(status: u32) -> Self {
        Self(status)
    }

    pub fn is_ok(&self) -> Result<(), Error> {
        Err(if self.0 & Self::OCDDSR_EXECEXCEPTION == 1 {
            Error::ExecExeception
        } else if self.0 & Self::OCDDSR_EXECBUSY == 1 {
            Error::ExecBusy
        } else if self.0 & Self::OCDDSR_EXECOVERRUN == 1 {
            Error::ExecOverrun
        } else if self.0 & Self::OCDDSR_DBGMODPOWERON == 0 { // should always be set to one
            Error::XdmPoweredOff
        } else {
            return Ok(())
        })
    }
}

impl Debug for DebugStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DSR: {:032b}", self.0)
    }
}
