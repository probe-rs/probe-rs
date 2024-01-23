//! A crate for talking to J-Link debug probes connected via USB.
//!
//! This crate allows access to the vendor-specific USB interface used to control JTAG / SWD
//! operations and other functionality. It does *not* provide access to the virtual COM port
//! functionality (which is a regular CDC device, so no special support is needed).
//!
//! Inspired by [libjaylink] (though this library is not a port).
//!
//! [libjaylink]: https://repo.or.cz/libjaylink.git
//!
//! # Pinout
//!
//! J-Link uses a pinout based on the standard 20-pin ARM JTAG connector, extended for SWD
//! compatibility and with pins for UART.
//!
//! JTAG pinout:
//!
//! ```notrust
//!            ┌───────────┐
//!     VTref  │ *  1  2 * │ NC
//!     nTRST  │ *  3  4 * │ GND
//!       TDI  │ *  5  6 * │ GND
//!       TMS  │ *  7  8 * │ GND
//!       TCK ┌┘ *  9 10 * │ GND
//!      RTCK └┐ * 11 12 * │ GND
//!       TDO  │ * 13 14 * │ GND
//!     RESET  │ * 15 16 * │ GND
//!     DBGRQ  │ * 17 18 * │ GND
//! 5V-Supply  │ * 19 20 * │ GND
//!            └───────────┘
//! ```
//!
//! SWD (+ UART) pinout:
//!
//! ```notrust
//!            ┌───────────┐
//!     VTref  │ *  1  2 * │ NC
//!         -  │ *  3  4 * │ GND
//! J-Link TX  │ *  5  6 * │ GND
//!     SWDIO  │ *  7  8 * │ GND
//!     SWCLK ┌┘ *  9 10 * │ GND
//!         - └┐ * 11 12 * │ GND
//!       SWO  │ * 13 14 * │ GND
//!     RESET  │ * 15 16 * │ GND
//! J-Link RX  │ * 17 18 * │ GND
//! 5V-Supply  │ * 19 20 * │ GND
//!            └───────────┘
//! ```
//!
//! PIC32 ICSP pinout (untested):
//!
//! ```notrust
//!            ┌───────────┐
//!     VTref  │ *  1  2 * │ NC
//!         -  │ *  3  4 * │ GND
//!         -  │ *  5  6 * │ GND
//!      PGED  │ *  7  8 * │ GND
//!      PGEC ┌┘ *  9 10 * │ GND
//!         - └┐ * 11 12 * │ GND
//!         -  │ * 13 14 * │ GND
//!     RESET  │ * 15 16 * │ GND
//!         -  │ * 17 18 * │ GND
//! 5V-Supply  │ * 19 20 * │ GND
//!            └───────────┘
//! ```
//!
//! # Reference
//!
//! Segger has released a PDF documenting the USB protocol: "Reference manual for J-Link USB
//! Protocol" (Document RM08001-R2).
//!
//! The archive.org version is the most up-to-date one.

#![warn(missing_debug_implementations, unreachable_pub)]
// We use explicit lifetimes to make APIs easier to understand (this also affects rustdoc)
#![allow(clippy::needless_lifetimes)]
#![allow(unreachable_pub)]
#![allow(unused)]

#[macro_use]
mod macros;
mod bits;
mod capabilities;
mod error;
mod interface;

use crate::probe::usb_util::InterfaceExt;

pub(crate) use self::bits::BitIter;
pub(crate) use self::capabilities::{Capabilities, Capability};
pub(crate) use self::error::{Error, ErrorKind};
pub(crate) use self::interface::{Interface, Interfaces};

use self::bits::IteratorExt as _;
use self::error::ResultExt as _;
use bitflags::bitflags;
use byteorder::{LittleEndian, ReadBytesExt};
use io::Cursor;
use nusb::transfer::{Direction, EndpointType};
use nusb::DeviceInfo;
use std::cell::{Cell, RefCell, RefMut};
use std::convert::{TryFrom, TryInto};
use std::time::{Duration, Instant};
use std::{
    cmp, fmt,
    io::{self, Read},
    ops::Deref,
    thread,
};
use tracing::{debug, trace, warn};

/// A result type with the error hardwired to [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

const VID_SEGGER: u16 = 0x1366;

const TIMEOUT_DEFAULT: Duration = Duration::from_millis(500);

#[repr(u8)]
#[allow(dead_code)]
enum Command {
    Version = 0x01,
    GetSpeeds = 0xC0,
    GetMaxMemBlock = 0xD4,
    GetCaps = 0xE8,
    GetCapsEx = 0xED,
    GetHwVersion = 0xF0,

    GetState = 0x07,
    GetHwInfo = 0xC1,
    GetCounters = 0xC2,
    MeasureRtckReact = 0xF6,

    ResetTrst = 0x02,
    SetSpeed = 0x05,
    SelectIf = 0xC7,
    SetKsPower = 0x08,
    HwClock = 0xC8,
    HwTms0 = 0xC9,
    HwTms1 = 0xCA,
    HwData0 = 0xCB,
    HwData1 = 0xCC,
    HwJtag = 0xCD,
    HwJtag2 = 0xCE,
    HwJtag3 = 0xCF,
    HwJtagWrite = 0xD5,
    HwJtagGetResult = 0xD6,
    HwTrst0 = 0xDE,
    HwTrst1 = 0xDF,
    Swo = 0xEB,
    WriteDcc = 0xF1,

    ResetTarget = 0x03,
    HwReleaseResetStopEx = 0xD0,
    HwReleaseResetStopTimed = 0xD1,
    HwReset0 = 0xDC,
    HwReset1 = 0xDD,
    GetCpuCaps = 0xE9,
    ExecCpuCmd = 0xEA,
    WriteMem = 0xF4,
    ReadMem = 0xF5,
    WriteMemArm79 = 0xF7,
    ReadMemArm79 = 0xF8,

    ReadConfig = 0xF2,
    WriteConfig = 0xF3,
}

#[repr(u8)]
enum SwoCommand {
    Start = 0x64,
    Stop = 0x65,
    Read = 0x66,
    GetSpeeds = 0x6E,
}

#[repr(u8)]
enum SwoParam {
    Mode = 0x01,
    Baudrate = 0x02,
    ReadSize = 0x03,
    BufferSize = 0x04,
    // FIXME: Do these have hardware/firmware version requirements to be recognized?
}

/// The supported SWO data encoding modes.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[repr(u32)]
#[non_exhaustive]
pub enum SwoMode {
    Uart = 0x00000000,
    // FIXME: Manchester encoding?
}

bitflags! {
    /// SWO status returned by probe on SWO buffer read.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    struct SwoStatus: u32 {
        /// The on-probe buffer has overflowed. Device data was lost.
        const OVERRUN = 1 << 0;
    }
}

impl SwoStatus {
    fn new(bits: u32) -> Self {
        let flags = SwoStatus::from_bits_truncate(bits);
        if flags.bits() != bits {
            warn!("Unknown SWO status flag bits: 0x{:08X}", bits);
        }
        flags
    }
}

/// A handle to a J-Link USB device.
///
/// This is the main interface type of this library. There are multiple ways of obtaining an
/// instance of it:
///
/// * [`JayLink::open_by_serial`]: Either opens the only J-Link device connected to the computer, or
///   opens a specific one by its serial number. Recommended for applications that interact with one
///   J-Link device only (ie. most of them).
/// * [`JayLink::open_usb`]: Opens a specific J-Link device according to the given
///   [`UsbDeviceInfo`]. Also see [`scan_usb`].
pub struct JayLink {
    handle: nusb::Interface,

    read_ep: u8,
    write_ep: u8,
    cmd_buf: RefCell<Vec<u8>>,

    /// The capabilities reported by the device. They're fetched once, when the device is opened.
    caps: Capabilities,

    /// The supported interfaces. Like `caps`, this is fetched once when opening the device.
    interfaces: Interfaces,

    /// The currently selected target interface. This is stored here to avoid unnecessary roundtrips
    /// when performing target I/O operations.
    interface: Interface,
}

impl JayLink {
    /// Opens a specific J-Link USB device.
    ///
    /// **Note**: Probes remember their selected interfaces between reconnections, so it is
    /// recommended to always call [`JayLink::select_interface`] after opening a probe.
    pub fn open_usb(usb_device: DeviceInfo) -> Result<Self> {
        fn open_error(e: std::io::Error, while_: &'static str) -> Error {
            let inner: Box<dyn std::error::Error + Send + Sync> = if cfg!(windows) {
                format!(
                    "{} (this error may be caused by not having the \
                        WinUSB driver installed; use Zadig (https://zadig.akeo.ie/) to install it \
                        for the J-Link device; this will replace the SEGGER J-Link driver)",
                    e
                )
                .into()
            } else {
                Box::new(e)
            };

            Error::with_while(ErrorKind::Usb, inner, while_)
        }

        let handle = usb_device
            .open()
            .map_err(|e| open_error(e, "opening USB device"))?;

        let configs: Vec<_> = handle.configurations().collect();

        if configs.len() != 1 {
            warn!("device has {} configurations, expected 1", configs.len());
        }

        let conf = &configs[0];
        debug!("scanning {} interfaces", conf.interfaces().count());
        trace!("active configuration descriptor: {:#x?}", conf);

        let mut jlink_intf = None;
        for intf in conf.interfaces() {
            trace!("interface #{} descriptors:", intf.interface_number());

            for descr in intf.alt_settings() {
                trace!("{:#x?}", descr);

                // We detect the proprietary J-Link interface using the vendor-specific class codes
                // and the endpoint properties
                if descr.class() == 0xff && descr.subclass() == 0xff && descr.protocol() == 0xff {
                    if let Some((intf, _, _)) = jlink_intf {
                        return Err(format!(
                            "found multiple matching USB interfaces ({} and {})",
                            intf,
                            descr.interface_number()
                        ))
                        .jaylink_err();
                    }

                    let endpoints: Vec<_> = descr.endpoints().collect();
                    trace!("endpoint descriptors: {:#x?}", endpoints);
                    if endpoints.len() != 2 {
                        warn!("vendor-specific interface with {} endpoints, expected 2 (skipping interface)", endpoints.len());
                        continue;
                    }

                    if !endpoints
                        .iter()
                        .all(|ep| ep.transfer_type() == EndpointType::Bulk)
                    {
                        warn!(
                            "encountered non-bulk endpoints, skipping interface: {:#x?}",
                            endpoints
                        );
                        continue;
                    }

                    let (read_ep, write_ep) = if endpoints[0].direction() == Direction::In {
                        (endpoints[0].address(), endpoints[1].address())
                    } else {
                        (endpoints[1].address(), endpoints[0].address())
                    };

                    jlink_intf = Some((descr.interface_number(), read_ep, write_ep));
                    debug!("J-Link interface is #{}", descr.interface_number());
                }
            }
        }

        let (intf, read_ep, write_ep) = if let Some(intf) = jlink_intf {
            intf
        } else {
            return Err("device is not a J-Link device".to_string()).jaylink_err();
        };

        let handle = handle
            .claim_interface(intf)
            .map_err(|e| open_error(e, "taking control over USB device"))?;

        let mut this = Self {
            read_ep,
            write_ep,
            cmd_buf: RefCell::new(Vec::new()),
            caps: Capabilities::from_raw_legacy(0), // dummy value
            interface: Interface::Spi,              // dummy value, must not be JTAG
            interfaces: Interfaces::from_bits_warn(0), // dummy value
            handle,
        };
        this.fill_capabilities()?;
        this.fill_interfaces()?;

        Ok(this)
    }

    /// Reads the advertised capabilities from the device.
    fn fill_capabilities(&mut self) -> Result<()> {
        self.write_cmd(&[Command::GetCaps as u8])?;

        let mut buf = [0; 4];
        self.read(&mut buf)?;

        let mut caps = Capabilities::from_raw_legacy(u32::from_le_bytes(buf));
        debug!("legacy caps: {:?}", caps);

        // If the `GET_CAPS_EX` capability is set, use the extended capability command to fetch
        // all the capabilities.
        if caps.contains(Capability::GetCapsEx) {
            self.write_cmd(&[Command::GetCapsEx as u8])?;

            let mut buf = [0; 32];
            self.read(&mut buf)?;
            let real_caps = Capabilities::from_raw_ex(buf);
            if !real_caps.contains_all(caps) {
                return Err(format!(
                    "ext. caps are not a superset of legacy caps (legacy: {:?}, ex: {:?})",
                    caps, real_caps
                ))
                .jaylink_err();
            }
            debug!("extended caps: {:?}", real_caps);
            caps = real_caps;
        } else {
            debug!("extended caps not supported");
        }

        self.caps = caps;
        Ok(())
    }

    fn fill_interfaces(&mut self) -> Result<()> {
        if !self.capabilities().contains(Capability::SelectIf) {
            // Pre-SELECT_IF probes only support JTAG.
            self.interfaces = Interfaces::single(Interface::Jtag);
            self.interface = Interface::Jtag;

            return Ok(());
        }

        self.write_cmd(&[Command::SelectIf as u8, 0xFF])?;

        let mut buf = [0; 4];
        self.read(&mut buf)?;

        let intfs = Interfaces::from_bits_warn(u32::from_le_bytes(buf));
        self.interfaces = intfs;
        Ok(())
    }

    fn buf(&self, len: usize) -> RefMut<'_, Vec<u8>> {
        let mut vec = self.cmd_buf.borrow_mut();
        vec.resize(len, 0);
        vec
    }

    fn write_cmd(&self, cmd: &[u8]) -> Result<()> {
        trace!("write {} bytes: {:x?}", cmd.len(), cmd);

        let n = self
            .handle
            .write_bulk(self.write_ep, cmd, TIMEOUT_DEFAULT)
            .jaylink_err_while("writing data to device")?;

        if n != cmd.len() {
            return Err(format!(
                "incomplete write (expected {} bytes, wrote {})",
                cmd.len(),
                n
            ))
            .jaylink_err();
        }
        Ok(())
    }

    fn read(&self, buf: &mut [u8]) -> Result<()> {
        let mut total = 0;

        while total < buf.len() {
            let n = self
                .handle
                .read_bulk(self.read_ep, &mut buf[total..], TIMEOUT_DEFAULT)
                .jaylink_err_while("reading from device")?;
            total += n;
        }

        trace!("read {} bytes: {:x?}", buf.len(), buf);

        Ok(())
    }

    fn require_capability(&self, cap: Capability) -> Result<()> {
        if self.capabilities().contains(cap) {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::MissingCapability,
                format!("device is missing capabilities ({:?}) for operation", cap),
            ))
        }
    }

    fn require_interface_supported(&self, intf: Interface) -> Result<()> {
        if self.interfaces.contains(intf) {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::InterfaceNotSupported,
                format!("probe does not support target interface {:?}", intf),
            ))
        }
    }

    fn require_interface_selected(&self, intf: Interface) -> Result<()> {
        if self.interface == intf {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::Other,
                format!("interface {} must be selected for this operation (currently using interface {})", intf, self.interface),
            ))
        }
    }

    /// Reads the firmware version string from the device.
    pub fn read_firmware_version(&self) -> Result<String> {
        self.write_cmd(&[Command::Version as u8])?;

        let mut buf = [0; 2];
        self.read(&mut buf)?;
        let num_bytes = u16::from_le_bytes(buf);
        let mut buf = self.buf(num_bytes.into());
        let buf = &mut buf[..usize::from(num_bytes)];
        self.read(buf)?;

        Ok(String::from_utf8_lossy(
            // The firmware version string returned may contain null bytes. If
            // this happens, only return the preceding bytes.
            match buf.iter().position(|&b| b == 0) {
                Some(pos) => &buf[..pos],
                None => buf,
            },
        )
        .into_owned())
    }

    /// Reads the hardware version from the device.
    ///
    /// This requires the probe to support [`Capability::GetHwVersion`].
    pub fn read_hardware_version(&self) -> Result<HardwareVersion> {
        self.require_capability(Capability::GetHwVersion)?;

        self.write_cmd(&[Command::GetHwVersion as u8])?;

        let mut buf = [0; 4];
        self.read(&mut buf)?;

        Ok(HardwareVersion::from_u32(u32::from_le_bytes(buf)))
    }

    /// Reads the probe's communication speed information about the currently selected interface.
    ///
    /// Supported speeds may differ between [`Interface`]s, so the right interface needs to be
    /// selected for the returned value to make sense.
    ///
    /// This requires the probe to support [`Capability::SpeedInfo`].
    pub fn read_speeds(&self) -> Result<SpeedInfo> {
        self.require_capability(Capability::SpeedInfo)?;

        self.write_cmd(&[Command::GetSpeeds as u8])?;

        let mut buf = [0; 6];
        self.read(&mut buf)?;
        let mut buf = &buf[..];

        Ok(SpeedInfo {
            base_freq: buf.read_u32::<LittleEndian>().unwrap(),
            min_div: buf.read_u16::<LittleEndian>().unwrap(),
        })
    }

    /// Reads the probe's SWO capture speed information.
    ///
    /// This requires the probe to support [`Capability::Swo`].
    pub fn read_swo_speeds(&self, mode: SwoMode) -> Result<SwoSpeedInfo> {
        self.require_capability(Capability::Swo)?;

        let mut buf = [0; 9];
        buf[0] = Command::Swo as u8;
        buf[1] = SwoCommand::GetSpeeds as u8;
        buf[2] = 0x04; // Next param has 4 data Bytes
        buf[3] = SwoParam::Mode as u8;
        buf[4..8].copy_from_slice(&(mode as u32).to_le_bytes());
        buf[8] = 0x00;

        self.write_cmd(&buf)?;

        let mut buf = [0; 28];
        self.read(&mut buf)?;

        let mut len = [0; 4];
        len.copy_from_slice(&buf[0..4]);
        let len = u32::from_le_bytes(len);
        if len != 28 {
            return Err(Error::new(
                ErrorKind::Other,
                format!("Unexpected response length {}, expected 28", len),
            ));
        }

        // Skip length and reserved word.
        // FIXME: What's the word after the length for?
        let mut buf = &buf[8..];

        Ok(SwoSpeedInfo {
            base_freq: buf.read_u32::<LittleEndian>().unwrap(),
            min_div: buf.read_u32::<LittleEndian>().unwrap(),
            max_div: buf.read_u32::<LittleEndian>().unwrap(),
            min_presc: buf.read_u32::<LittleEndian>().unwrap(),
            max_presc: buf.read_u32::<LittleEndian>().unwrap(),
        })
    }

    /// Reads the maximum mem block size in Bytes.
    ///
    /// This requires the probe to support [`Capability::GetMaxBlockSize`].
    pub fn read_max_mem_block(&self) -> Result<u32> {
        // This cap refers to a nonexistent command `GET_MAX_BLOCK_SIZE`, but it probably means
        // `GET_MAX_MEM_BLOCK`.
        self.require_capability(Capability::GetMaxBlockSize)?;

        self.write_cmd(&[Command::GetMaxMemBlock as u8])?;

        let mut buf = [0; 4];
        self.read(&mut buf)?;

        Ok(u32::from_le_bytes(buf))
    }

    /// Returns the capabilities advertised by the probe.
    pub fn capabilities(&self) -> Capabilities {
        self.caps
    }

    /// Returns the set of target interfaces supported by the probe.
    pub fn available_interfaces(&self) -> Interfaces {
        self.interfaces
    }

    /// Reads the currently selected target interface.
    ///
    /// **Note**: There is no guarantee that the returned interface is actually supported (ie. it
    /// might not be in the list returned by [`JayLink::available_interfaces`]). In particular, some
    /// embedded J-Link probes start up with JTAG selected, but only support SWD.
    pub fn current_interface(&self) -> Interface {
        self.interface
    }

    /// Selects the interface to use for talking to the target MCU.
    ///
    /// Switching interfaces will reset the configured transfer speed, so [`JayLink::set_speed`]
    /// needs to be called *after* `select_interface`.
    ///
    /// This requires the probe to support [`Capability::SelectIf`].
    ///
    /// **Note**: Selecting a different interface may cause the J-Link to perform target I/O!
    pub fn select_interface(&mut self, intf: Interface) -> Result<()> {
        if self.interface == intf {
            return Ok(());
        }

        self.require_capability(Capability::SelectIf)?;

        self.require_interface_supported(intf)?;

        self.write_cmd(&[Command::SelectIf as u8, intf.as_u8()])?;

        // Returns the previous interface, ignore it
        let mut buf = [0; 4];
        self.read(&mut buf)?;

        self.interface = intf;

        Ok(())
    }

    /// Changes the state of the TMS / SWDIO pin (pin 7).
    ///
    /// The pin will be set to the level of `VTref` if `tms` is `true`, and to GND if it is `false`.
    ///
    /// **Note**: On some hardware, detaching `VTref` might not affect the internal reading, so the
    /// old level might still be used afterwards.
    pub fn set_tms(&mut self, tms: bool) -> Result<()> {
        let cmd = if tms {
            Command::HwTms1
        } else {
            Command::HwTms0
        };
        self.write_cmd(&[cmd as u8])
    }

    /// Changes the state of the TDI / TX pin (pin 5).
    ///
    /// The pin will be set to the level of `VTref` if `tdi` is `true`, and to GND if it is `false`.
    ///
    /// **Note**: On some hardware, detaching `VTref` might not affect the internal reading, so the
    /// old level might still be used afterwards.
    pub fn set_tdi(&mut self, tdi: bool) -> Result<()> {
        let cmd = if tdi {
            Command::HwData1
        } else {
            Command::HwData0
        };
        self.write_cmd(&[cmd as u8])
    }

    /// Changes the state of the (n)TRST pin (pin 3).
    ///
    /// The pin will be set to the level of `VTref` if `trst` is `true`, and to GND if it is
    /// `false`.
    ///
    /// **Note**: On some hardware, detaching `VTref` might not affect the internal reading, so the
    /// old level might still be used afterwards.
    ///
    /// **Note**: Some embedded J-Link probes may not expose this pin or may not allow controlling
    /// it using this function.
    pub fn set_trst(&mut self, trst: bool) -> Result<()> {
        let cmd = if trst {
            Command::HwTrst1
        } else {
            Command::HwTrst0
        };
        self.write_cmd(&[cmd as u8])
    }

    /// Changes the state of the RESET pin (pin 15).
    ///
    /// RESET is an open-collector / open-drain output. If `reset` is `true`, the output will float.
    /// If `reset` is `false`, the output will be pulled to ground.
    ///
    /// **Note**: Some embedded J-Link probes may not expose this pin or may not allow controlling
    /// it using this function.
    pub fn set_reset(&mut self, reset: bool) -> Result<()> {
        let cmd = if reset {
            Command::HwReset1
        } else {
            Command::HwReset0
        };
        self.write_cmd(&[cmd as u8])
    }

    /// Resets the target's JTAG TAP controller by temporarily asserting (n)TRST (Pin 3).
    ///
    /// This might not do anything if the pin is not connected to the target. It does not affect
    /// non-JTAG target interfaces.
    pub fn reset_trst(&mut self) -> Result<()> {
        self.write_cmd(&[Command::ResetTrst as u8])
    }

    /// Resets the target by temporarily asserting the RESET pin (pin 15).
    ///
    /// This might not do anything if the RESET pin is not connected to the target.
    pub fn reset_target(&mut self) -> Result<()> {
        self.write_cmd(&[Command::ResetTarget as u8])
    }

    /// Sets the target communication speed.
    ///
    /// If `speed` is set to [`SpeedConfig::ADAPTIVE`], then the probe has to support
    /// [`Capability::AdaptiveClocking`]. Note that adaptive clocking may not work for all target
    /// interfaces (eg. SWD).
    ///
    /// When the selected target interface is switched (by calling [`JayLink::select_interface`], or
    /// any API method that automatically selects an interface), the communication speed is reset to
    /// some unspecified default value.
    pub fn set_speed(&mut self, speed: SpeedConfig) -> Result<()> {
        if speed.raw == SpeedConfig::ADAPTIVE.raw {
            self.require_capability(Capability::AdaptiveClocking)?;
        }

        let mut buf = [Command::SetSpeed as u8, 0, 0];
        buf[1..3].copy_from_slice(&speed.raw.to_le_bytes());
        self.write_cmd(&buf)?;

        Ok(())
    }

    /// Reads the target voltage measured on the `VTref` pin, in millivolts.
    ///
    /// In order to use the J-Link, this voltage must be present, since it will be used as the level
    /// of the I/O signals to the target.
    pub fn read_target_voltage(&self) -> Result<u16> {
        self.write_cmd(&[Command::GetState as u8])?;

        let mut buf = [0; 8];
        self.read(&mut buf)?;

        let voltage = [buf[0], buf[1]];
        Ok(u16::from_le_bytes(voltage))
    }

    /// Enables or disables the 5V Power supply on pin 19.
    ///
    /// This requires the probe to support [`Capability::SetKsPower`].
    ///
    /// **Note**: The startup state of the power supply can be configured in non-volatile memory.
    ///
    /// **Note**: Some embedded J-Links may not provide this feature or do not have the 5V supply
    /// routed to a pin. In that case this function might return an error, or it might return
    /// successfully, but without doing anything.
    ///
    /// **Note**: The 5V supply is protected against overcurrent. Check the device manual for more
    /// information on this.
    ///
    /// [`SET_KS_POWER`]: Capabilities::SET_KS_POWER
    pub fn set_kickstart_power(&mut self, enable: bool) -> Result<()> {
        self.require_capability(Capability::SetKsPower)?;
        self.write_cmd(&[Command::SetKsPower as u8, enable as u8])?;
        Ok(())
    }

    /// Performs a JTAG I/O operation.
    ///
    /// This will shift out data on `TMS` (pin 7) and `TDI` (pin 5), while reading data shifted
    /// into `TDO` (pin 13).
    ///
    /// The data received on `TDO` is returned to the caller as an iterator yielding `bool`s.
    ///
    /// The caller must ensure that the probe is in JTAG mode by calling
    /// [`JayLink::select_interface`]`(`[`Interface::Jtag`]`)`.
    ///
    /// # Parameters
    ///
    /// * `tms`: TMS bits to transmit.
    /// * `tdi`: TDI bits to transmit.
    ///
    /// # Panics
    ///
    /// This method will panic if `tms` and `tdi` have different lengths. It will also panic if any
    /// of them contains more then 65535 bits of data, which is the maximum amount that can be
    /// transferred in one operation.
    ///
    // NB: Explicit `'a` lifetime used to improve rustdoc output
    pub fn jtag_io<'a, M, D>(&'a mut self, tms: M, tdi: D) -> Result<BitIter<'a>>
    where
        M: IntoIterator<Item = bool>,
        D: IntoIterator<Item = bool>,
    {
        self.require_interface_selected(Interface::Jtag)?;

        let mut has_status_byte = false;
        // There's 3 commands for doing a JTAG transfer. The older 2 are obsolete with hardware
        // version 5 and above, which adds the 3rd command. Unfortunately we cannot reliably use the
        // HW version to determine this since some embedded J-Link probes have a HW version of
        // 1.0.0, but still support SWD, so we use the `SELECT_IF` capability instead.
        let cmd = if self.capabilities().contains(Capability::SelectIf) {
            // Use the new JTAG3 command, make sure to select the JTAG interface mode
            self.select_interface(Interface::Jtag)?;
            has_status_byte = true;
            Command::HwJtag3
        } else {
            // Use the legacy JTAG2 command
            // FIXME is HW_JTAG relevant at all?
            Command::HwJtag2
        };

        // Collect the bit iterators into the buffer. We don't know the length in advance.
        let tms = tms.into_iter();
        let tdi = tdi.into_iter();
        let bit_count_hint = cmp::max(tms.size_hint().0, tdi.size_hint().0);
        let capacity = 1 + 1 + 2 + ((bit_count_hint + 7) / 8) * 2;
        let mut buf = self.buf(capacity);
        buf.resize(4, 0);
        buf[0] = cmd as u8;
        // buf[1] is dummy data for alignment
        // buf[2..=3] is the bit count, which we'll fill in later
        let mut tms_bit_count = 0;
        buf.extend(tms.inspect(|_| tms_bit_count += 1).collapse_bytes());
        let mut tdi_bit_count = 0;
        buf.extend(tdi.inspect(|_| tdi_bit_count += 1).collapse_bytes());

        assert_eq!(
            tms_bit_count, tdi_bit_count,
            "TMS and TDI must have the same number of bits"
        );

        let bit_count = u16::try_from(tms_bit_count).expect("too much data to transfer");

        // JTAG3 and JTAG2 use the same format for JTAG operations
        buf[2..=3].copy_from_slice(&bit_count.to_le_bytes());

        self.write_cmd(&buf)?;

        // Round bit count up to multple of 8 to get the number of response bytes.
        let num_resp_bytes = (tms_bit_count + 7) / 8;
        trace!(
            "{} TMS/TDI bits sent; reading {} response bytes",
            tms_bit_count,
            num_resp_bytes
        );

        // Response is `num_resp_bytes` TDO data bytes and one status byte,
        // if the JTAG3 command is used.
        let mut read_len = num_resp_bytes;

        if has_status_byte {
            read_len += 1;
        }

        self.read(&mut buf[..read_len])?;

        // Check the status if a JTAG3 command was used.
        if has_status_byte && buf[read_len - 1] != 0 {
            return Err(Error::new(
                ErrorKind::Other,
                format!(
                    "probe I/O command returned error code {:#x}",
                    buf[read_len - 1]
                ),
            ));
        }

        drop(buf);

        Ok(BitIter::new(
            &self.cmd_buf.get_mut()[..num_resp_bytes],
            tms_bit_count,
        ))
    }

    /// Performs an SWD I/O operation.
    ///
    /// This requires the probe to support [`Capability::SelectIf`] and support for
    /// [`Interface::Swd`].
    ///
    /// The caller must ensure that the probe is in SWD mode by calling
    /// [`JayLink::select_interface`]`(`[`Interface::Swd`]`)`.
    ///
    /// # Parameters
    ///
    /// * `dir`: Transfer directions of the `swdio` bits (`false` = 0 = Input, `true` = 1 = Output).
    /// * `swdio`: SWD data bits.
    ///
    /// If `dir` is `true`, the corresponding bit in `swdio` will be written to the target; if it is
    /// `false`, the bit in `swdio` is ignored and a bit is read from the target instead.
    ///
    /// # Return Value
    ///
    /// An iterator over the `SWDIO` bits is returned. Bits that were sent to the target (where
    /// `dir` = `true`) are undefined, and bits that were read from the target (`dir` = `false`)
    /// will have whatever value the target sent.
    // NB: Explicit `'a` lifetime used to improve rustdoc output
    pub fn swd_io<'a, D, S>(&'a mut self, dir: D, swdio: S) -> Result<BitIter<'a>>
    where
        D: IntoIterator<Item = bool>,
        S: IntoIterator<Item = bool>,
    {
        self.require_interface_selected(Interface::Swd)?;

        // Collect the bit iterators into the buffer. We don't know the length in advance.
        let dir = dir.into_iter();
        let swdio = swdio.into_iter();
        let bit_count_hint = cmp::max(dir.size_hint().0, swdio.size_hint().0);
        let capacity = 1 + 1 + 2 + ((bit_count_hint + 7) / 8) * 2;
        let mut buf = self.buf(capacity);
        buf.resize(4, 0);
        buf[0] = Command::HwJtag3 as u8;
        buf[1] = 0;
        // buf[1] is dummy data for alignment
        // buf[2..=3] is the bit count, which we'll fill in later
        let mut dir_bit_count = 0;
        buf.extend(dir.inspect(|_| dir_bit_count += 1).collapse_bytes());
        let mut swdio_bit_count = 0;
        buf.extend(swdio.inspect(|_| swdio_bit_count += 1).collapse_bytes());

        assert_eq!(
            dir_bit_count, swdio_bit_count,
            "`dir` and `swdio` must have the same number of bits"
        );
        assert!(dir_bit_count < 65535, "too much data to transfer");

        let num_bits = dir_bit_count as u16;
        buf[2..=3].copy_from_slice(&num_bits.to_le_bytes());
        let num_bytes = usize::from((num_bits + 7) >> 3);

        self.write_cmd(&buf)?;

        // Response is `num_bytes` SWDIO data bytes and one status byte
        self.read(&mut buf[..num_bytes + 1])?;

        if buf[num_bytes] != 0 {
            return Err(format!(
                "probe I/O command returned error code {:#x}",
                buf[num_bytes]
            ))
            .jaylink_err();
        }

        drop(buf);

        Ok(BitIter::new(
            &self.cmd_buf.get_mut()[..num_bytes],
            dir_bit_count,
        ))
    }

    /// Starts capturing SWO data.
    ///
    /// This will switch the probe to SWD interface mode if necessary (required for SWO capture).
    ///
    /// Requires the probe to support [`Capability::Swo`].
    ///
    /// # Parameters
    ///
    /// - `mode`: The SWO data encoding mode to use.
    /// - `speed`: The data rate to capture at (when using [`SwoMode::Uart`], this is the UART baud
    ///   rate).
    /// - `buf_size`: The size (in Bytes) of the on-device buffer to allocate for the SWO data. You
    ///   can call [`JayLink::read_max_mem_block`] to get an approximation of the available memory
    ///   on the probe.
    ///
    /// # Return Value
    ///
    /// This returns a [`SwoStream`] object, which can be used to directly read the captured SWO
    /// data via [`std::io::Read`]. If blocking reads are undesired (or the [`JayLink`] instance
    /// needs to be used for something else while SWO capture is in progress), the [`SwoStream`]
    /// can be ignored and [`JayLink::swo_read`] be used instead.
    pub fn swo_start<'a>(
        &'a mut self,
        mode: SwoMode,
        speed: u32,
        buf_size: u32,
    ) -> Result<SwoStream<'a>> {
        self.require_capability(Capability::Swo)?;

        // The probe must be in SWD mode for SWO capture to work.
        self.require_interface_selected(Interface::Swd)?;

        let mut buf = [0; 21];
        buf[0] = Command::Swo as u8;
        buf[1] = SwoCommand::Start as u8;
        buf[2] = 0x04;
        buf[3] = SwoParam::Mode as u8;
        buf[4..8].copy_from_slice(&(mode as u32).to_le_bytes());
        buf[8] = 0x04;
        buf[9] = SwoParam::Baudrate as u8;
        buf[10..14].copy_from_slice(&speed.to_le_bytes());
        buf[14] = 0x04;
        buf[15] = SwoParam::BufferSize as u8;
        buf[16..20].copy_from_slice(&buf_size.to_le_bytes());
        buf[20] = 0x00;

        self.write_cmd(&buf)?;

        let mut status = [0; 4];
        self.read(&mut status)?;
        let status = SwoStatus::new(u32::from_le_bytes(status));

        Ok(SwoStream {
            jaylink: self,
            speed,
            buf_size,
            buf: Cursor::new(Vec::new()),
            next_poll: Instant::now(),
            status: Cell::new(status),
        })
    }

    /// Stops capturing SWO data.
    pub fn swo_stop(&mut self) -> Result<()> {
        self.require_capability(Capability::Swo)?;

        let buf = [
            Command::Swo as u8,
            SwoCommand::Stop as u8,
            0x00, // no parameters
        ];

        self.write_cmd(&buf)?;

        let mut status = [0; 4];
        self.read(&mut status)?;
        let _status = SwoStatus::new(u32::from_le_bytes(status));
        // FIXME: What to do with the status?

        Ok(())
    }

    /// Reads captured SWO data from the probe and writes it to `data`.
    ///
    /// This needs to be called regularly after SWO capturing has been started. If it is not called
    /// often enough, the buffer on the probe will fill up and device data will be dropped. You can
    /// call [`SwoData::did_overrun`] to check for this condition.
    ///
    /// **Note**: the probe firmware seems to dislike many short SWO reads (as in, the probe will
    /// *fall off the bus and reset*), so it is recommended to use a buffer that is the same size as
    /// the on-probe data buffer.
    pub fn swo_read<'a>(&self, data: &'a mut [u8]) -> Result<SwoData<'a>> {
        let mut cmd = [0; 9];
        cmd[0] = Command::Swo as u8;
        cmd[1] = SwoCommand::Read as u8;
        cmd[2] = 0x04;
        cmd[3] = SwoParam::ReadSize as u8;
        cmd[4..8].copy_from_slice(&(data.len() as u32).to_le_bytes());
        cmd[8] = 0x00;

        self.write_cmd(&cmd)?;

        let mut header = [0; 8];
        self.read(&mut header)?;

        let status = {
            let mut status = [0; 4];
            status.copy_from_slice(&header[0..4]);
            let bits = u32::from_le_bytes(status);
            SwoStatus::new(bits)
        };
        let length = {
            let mut length = [0; 4];
            length.copy_from_slice(&header[4..8]);
            u32::from_le_bytes(length)
        };

        if status.contains(SwoStatus::OVERRUN) {
            warn!("SWO probe buffer overrun");
        }

        let len = length as usize;
        let buf = &mut data[..len];
        self.read(buf)?;

        Ok(SwoData { data: buf, status })
    }
}

impl fmt::Debug for JayLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JayLink").finish()
    }
}

/// A SWO data stream that implements [`std::io::Read`].
///
/// This is one way to consume SWO data. The other is to call [`JayLink::swo_read`] after SWO
/// capturing has been started.
///
/// Reading from this stream will block until some data is captured by the probe.
#[derive(Debug)]
pub struct SwoStream<'a> {
    jaylink: &'a JayLink,
    speed: u32,
    buf_size: u32,
    next_poll: Instant,
    /// Internal buffer the size of the on-probe buffer. This is filled in one go to avoid
    /// performing small reads which may crash the probe.
    buf: Cursor<Vec<u8>>,
    /// Accumulated SWO errors.
    status: Cell<SwoStatus>,
}

impl SwoStream<'_> {
    /// Returns whether the probe-internal buffer overflowed at some point, and clears the flag.
    ///
    /// This indicates that some device data was lost, and should be communicated to the end-user.
    pub fn did_overrun(&self) -> bool {
        let did = self.status.get().contains(SwoStatus::OVERRUN);
        self.status.set(self.status.get() & !SwoStatus::OVERRUN);
        did
    }

    /// Computes the suggested polling interval to avoid buffer overruns.
    fn poll_interval(&self) -> Duration {
        const MULTIPLIER: u32 = 2;

        let bytes_per_sec = self.speed / 8;
        let buffers_per_sec =
            cmp::max(1, bytes_per_sec / self.buf.get_ref().len() as u32) * MULTIPLIER;
        Duration::from_micros(1_000_000 / u64::from(buffers_per_sec))
    }
}

fn to_io_error(error: Error) -> io::Error {
    io::Error::new(io::ErrorKind::Other, error)
}

impl<'a> Read for SwoStream<'a> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.buf.position() == self.buf.get_ref().len() as u64 {
            // At end of buffer. (Blocking) Refill.
            self.buf.get_mut().resize(self.buf_size as usize, 0);
            loop {
                // If we have recently polled, wait until the next poll is useful to avoid 100% CPU
                // usage.
                let now = Instant::now();
                if now < self.next_poll {
                    thread::sleep(self.next_poll - now);
                }

                let buf = self.buf.get_mut();
                let data = self.jaylink.swo_read(buf).map_err(to_io_error)?;
                self.status.set(self.status.get() | data.status);
                let len = data.len();

                // Since `self.buf` is the same length as the on-probe buffer, the probe buffer is
                // now empty and we can wait `self.poll_interval()` until the next read.
                self.next_poll += self.poll_interval();

                if len != 0 {
                    // There's now *some* data in the buffer.
                    self.buf.get_mut().truncate(len);
                    self.buf.set_position(0);
                    break;
                }

                // If `data.len() == 0`, no data from the target has arrived. Since we can't return 0
                // bytes (it indicates the end of the stream, in reality the stream is just very slow),
                // we just loop (and sleep appropriately to not waste CPU).
            }
        }

        self.buf.read(buf)
    }
}

/// SWO data that was read via [`JayLink::swo_read`].
#[derive(Debug)]
pub struct SwoData<'a> {
    data: &'a [u8],
    status: SwoStatus,
}

impl<'a> SwoData<'a> {
    /// Returns whether the probe-internal buffer overflowed before the last read.
    ///
    /// This indicates that some device data was lost.
    pub fn did_overrun(&self) -> bool {
        self.status.contains(SwoStatus::OVERRUN)
    }
}

impl<'a> AsRef<[u8]> for SwoData<'a> {
    fn as_ref(&self) -> &[u8] {
        self.data
    }
}

impl<'a> Deref for SwoData<'a> {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        self.data
    }
}

/// A hardware version returned by [`JayLink::read_hardware_version`].
///
/// Note that the reported hardware version does not allow reliable feature detection, since
/// embedded J-Link probes might return a hardware version of 1.0.0 despite supporting SWD and other
/// much newer features.
#[derive(Debug)]
pub struct HardwareVersion(u32);

impl HardwareVersion {
    fn from_u32(raw: u32) -> Self {
        HardwareVersion(raw)
    }

    /// Returns the type of hardware (or `None` if the hardware type is unknown).
    pub fn hardware_type(&self) -> Option<HardwareType> {
        Some(match (self.0 / 1000000) % 100 {
            0 => HardwareType::JLink,
            1 => HardwareType::JTrace,
            2 => HardwareType::Flasher,
            3 => HardwareType::JLinkPro,
            _ => return None,
        })
    }

    /// The major version.
    pub fn major(&self) -> u8 {
        // Decimal coded Decimal, cool cool
        (self.0 / 10000) as u8
    }

    /// The minor version.
    pub fn minor(&self) -> u8 {
        ((self.0 % 10000) / 100) as u8
    }

    /// The hardware revision.
    pub fn revision(&self) -> u8 {
        (self.0 % 100) as u8
    }
}

impl fmt::Display for HardwareVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(hw) = self.hardware_type() {
            write!(f, "{} ", hw)?;
        }
        write!(f, "{}.{}.{}", self.major(), self.minor(), self.revision())
    }
}

/// The hardware/product type of the device.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HardwareType {
    JLink,
    JTrace,
    Flasher,
    JLinkPro,
}

impl fmt::Display for HardwareType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            HardwareType::JLink => "J-Link",
            HardwareType::JTrace => "J-Trace",
            HardwareType::Flasher => "J-Flash",
            HardwareType::JLinkPro => "J-Link Pro",
        })
    }
}

/// J-Link communication speed info.
#[derive(Debug)]
pub struct SpeedInfo {
    base_freq: u32,
    min_div: u16,
}

impl SpeedInfo {
    /// Returns the maximum supported speed for target communication (in Hz).
    pub fn max_speed_hz(&self) -> u32 {
        self.base_freq / u32::from(self.min_div)
    }

    /// Returns a `SpeedConfig` that configures the fastest supported speed.
    pub fn max_speed_config(&self) -> SpeedConfig {
        let khz = cmp::min(self.max_speed_hz() / 1000, 0xFFFE);
        SpeedConfig::khz(khz.try_into().unwrap()).unwrap()
    }
}

/// Supported SWO capture speed info.
#[derive(Debug)]
pub struct SwoSpeedInfo {
    base_freq: u32,
    min_div: u32,
    #[allow(dead_code)]
    max_div: u32,

    min_presc: u32,
    #[allow(dead_code)]
    max_presc: u32,
}

impl SwoSpeedInfo {
    /// Returns the maximum supported speed for SWO capture (in Hz).
    pub fn max_speed_hz(&self) -> u32 {
        self.base_freq / self.min_div / cmp::max(1, self.min_presc)
    }
}

/// Target communication speed setting.
///
/// This determines the clock frequency of the target communication. Supported speeds for the
/// currently selected target interface can be fetched via [`JayLink::read_speeds`].
#[derive(Debug, Copy, Clone)]
pub struct SpeedConfig {
    raw: u16,
}

impl SpeedConfig {
    /// Let the J-Link probe decide the speed.
    ///
    /// Requires the probe to support [`Capability::AdaptiveClocking`].
    pub const ADAPTIVE: Self = Self { raw: 0xFFFF };

    /// Manually specify speed in kHz.
    ///
    /// Returns `None` if the value is the invalid value `0xFFFF`. Note that this doesn't mean that
    /// every other value will be accepted by the device.
    pub fn khz(khz: u16) -> Option<Self> {
        if khz == 0xFFFF {
            None
        } else {
            Some(Self { raw: khz })
        }
    }
}

impl fmt::Display for SpeedConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.raw == Self::ADAPTIVE.raw {
            f.write_str("adaptive")
        } else {
            write!(f, "{} kHz", self.raw)
        }
    }
}

/// Scans for J-Link USB devices.
///
/// The returned iterator will yield all devices made by Segger, without filtering the product ID.
pub fn scan_usb() -> Result<impl Iterator<Item = DeviceInfo>> {
    Ok(nusb::list_devices()
        .jaylink_err()?
        .filter(|dev| dev.vendor_id() == VID_SEGGER))
}
