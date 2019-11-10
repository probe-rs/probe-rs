pub mod constants;
pub mod memory_interface;
pub mod tools;
mod usb_interface;

pub use self::usb_interface::STLinkUSBDevice;

use crate::coresight::ap_access::AccessPort;
use crate::probe::debug_probe::{DebugProbeInfo, Port};
use scroll::{Pread, BE};

use crate::probe::debug_probe::{DAPAccess, DebugProbe, DebugProbeError};
use crate::probe::protocol::WireProtocol;

use constants::{commands, JTagFrequencyToDivider, Status, SwdFrequencyToDelayCount};
use usb_interface::TIMEOUT;

pub struct STLink {
    device: STLinkUSBDevice,
    hw_version: u8,
    jtag_version: u8,
    protocol: WireProtocol,
}

impl DebugProbe for STLink {
    fn new_from_probe_info(info: &DebugProbeInfo) -> Result<Box<Self>, DebugProbeError>
    where
        Self: Sized,
    {
        let mut stlink = Self {
            device: STLinkUSBDevice::new_from_info(info)?,
            hw_version: 0,
            jtag_version: 0,
            protocol: WireProtocol::Swd,
        };

        stlink.init()?;

        Ok(Box::new(stlink))
    }

    fn get_name(&self) -> &str {
        "ST-Link"
    }

    /// Enters debug mode.
    fn attach(&mut self, protocol: Option<WireProtocol>) -> Result<WireProtocol, DebugProbeError> {
        self.enter_idle()?;

        let (param, protocol) = if let Some(protocol) = protocol {
            (
                match protocol {
                    WireProtocol::Jtag => commands::JTAG_ENTER_JTAG_NO_CORE_RESET,
                    WireProtocol::Swd => commands::JTAG_ENTER_SWD,
                },
                protocol,
            )
        } else {
            (commands::JTAG_ENTER_SWD, WireProtocol::Swd)
        };

        let mut buf = [0; 2];
        self.device.write(
            vec![commands::JTAG_COMMAND, commands::JTAG_ENTER2, param, 0],
            &[],
            &mut buf,
            TIMEOUT,
        )?;
        Self::check_status(&buf).and_then(|_| {
            // After we checked the status with success,
            // We store the current protocol.
            self.protocol = protocol;
            Ok(protocol)
        })
    }

    /// Leave debug mode.
    fn detach(&mut self) -> Result<(), DebugProbeError> {
        self.enter_idle()
    }

    /// Asserts the nRESET pin.
    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        let mut buf = [0; 2];
        self.device.write(
            vec![
                commands::JTAG_COMMAND,
                commands::JTAG_DRIVE_NRST,
                commands::JTAG_DRIVE_NRST_PULSE,
            ],
            &[],
            &mut buf,
            TIMEOUT,
        )?;
        Self::check_status(&buf)
    }
}

impl DAPAccess for STLink {
    /// Reads the DAP register on the specified port and address.
    fn read_register(&mut self, port: Port, addr: u16) -> Result<u32, DebugProbeError> {
        if (addr & 0xf0) == 0 || port != Port::DebugPort {
            let port = match port {
                Port::DebugPort => 0xffff,
                Port::AccessPort(p) => p,
            };

            let cmd = vec![
                commands::JTAG_COMMAND,
                commands::JTAG_READ_DAP_REG,
                (port & 0xFF) as u8,
                ((port >> 8) & 0xFF) as u8,
                (addr & 0xFF) as u8,
                ((addr >> 8) & 0xFF) as u8,
            ];
            let mut buf = [0; 8];
            self.device.write(cmd, &[], &mut buf, TIMEOUT)?;
            Self::check_status(&buf)?;
            // Unwrap is ok!
            Ok((&buf[4..8]).pread(0).unwrap())
        } else {
            Err(DebugProbeError::BlanksNotAllowedOnDPRegister)
        }
    }

    /// Writes a value to the DAP register on the specified port and address.
    fn write_register(&mut self, port: Port, addr: u16, value: u32) -> Result<(), DebugProbeError> {
        if (addr & 0xf0) == 0 || port != Port::DebugPort {
            let port = match port {
                Port::DebugPort => 0xffff,
                Port::AccessPort(p) => p,
            };

            let cmd = vec![
                commands::JTAG_COMMAND,
                commands::JTAG_WRITE_DAP_REG,
                (port & 0xFF) as u8,
                ((port >> 8) & 0xFF) as u8,
                (addr & 0xFF) as u8,
                ((addr >> 8) & 0xFF) as u8,
                (value & 0xFF) as u8,
                ((value >> 8) & 0xFF) as u8,
                ((value >> 16) & 0xFF) as u8,
                ((value >> 24) & 0xFF) as u8,
            ];
            let mut buf = [0; 2];
            self.device.write(cmd, &[], &mut buf, TIMEOUT)?;
            Self::check_status(&buf)?;
            Ok(())
        } else {
            Err(DebugProbeError::BlanksNotAllowedOnDPRegister)
        }
    }
}

impl Drop for STLink {
    fn drop(&mut self) {
        // We ignore the error case as we can't do much about it anyways.
        let _ = self.enter_idle();
    }
}

impl STLink {
    /// Maximum number of bytes to send or receive for 32- and 16- bit transfers.
    ///
    /// 8-bit transfers have a maximum size of the maximum USB packet size (64 bytes for full speed).
    const _MAXIMUM_TRANSFER_SIZE: u32 = 1024;

    /// Minimum required STLink firmware version.
    const MIN_JTAG_VERSION: u8 = 24;

    /// Firmware version that adds 16-bit transfers.
    const _MIN_JTAG_VERSION_16BIT_XFER: u8 = 26;

    /// Firmware version that adds multiple AP support.
    const MIN_JTAG_VERSION_MULTI_AP: u8 = 28;

    /// Reads the target voltage.
    /// For the china fake variants this will always read a nonzero value!
    pub fn get_target_voltage(&mut self) -> Result<f32, DebugProbeError> {
        let mut buf = [0; 8];
        match self
            .device
            .write(vec![commands::GET_TARGET_VOLTAGE], &[], &mut buf, TIMEOUT)
        {
            Ok(_) => {
                // The next two unwraps are safe!
                let a0 = (&buf[0..4]).pread::<u32>(0).unwrap() as f32;
                let a1 = (&buf[4..8]).pread::<u32>(0).unwrap() as f32;
                if a0 != 0.0 {
                    Ok((2.0 * a1 * 1.2 / a0) as f32)
                } else {
                    // Should never happen
                    Err(DebugProbeError::VoltageDivisionByZero)
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Commands the ST-Link to enter idle mode.
    /// Internal helper.
    fn enter_idle(&mut self) -> Result<(), DebugProbeError> {
        let mut buf = [0; 2];
        match self
            .device
            .write(vec![commands::GET_CURRENT_MODE], &[], &mut buf, TIMEOUT)
        {
            Ok(_) => {
                if buf[0] == commands::DEV_DFU_MODE {
                    self.device.write(
                        vec![commands::DFU_COMMAND, commands::DFU_EXIT],
                        &[],
                        &mut [],
                        TIMEOUT,
                    )
                } else if buf[0] == commands::DEV_JTAG_MODE {
                    self.device.write(
                        vec![commands::JTAG_COMMAND, commands::JTAG_EXIT],
                        &[],
                        &mut [],
                        TIMEOUT,
                    )
                } else if buf[0] == commands::DEV_SWIM_MODE {
                    self.device.write(
                        vec![commands::SWIM_COMMAND, commands::SWIM_EXIT],
                        &[],
                        &mut [],
                        TIMEOUT,
                    )
                } else {
                    Ok(())
                    // TODO: Look this up
                    // Err(DebugProbeError::UnknownMode)
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Reads the ST-Links version.
    /// Returns a tuple (hardware version, firmware version).
    /// This method stores the version data on the struct to make later use of it.
    fn get_version(&mut self) -> Result<(u8, u8), DebugProbeError> {
        const HW_VERSION_SHIFT: u8 = 12;
        const HW_VERSION_MASK: u8 = 0x0F;
        const JTAG_VERSION_SHIFT: u8 = 6;
        const JTAG_VERSION_MASK: u8 = 0x3F;
        // GET_VERSION response structure:
        //   Byte 0-1:
        //     [15:12] Major/HW version
        //     [11:6]  JTAG/SWD version
        //     [5:0]   SWIM or MSC version
        //   Byte 2-3: ST_VID
        //   Byte 4-5: STLINK_PID
        let mut buf = [0; 6];
        match self
            .device
            .write(vec![commands::GET_VERSION], &[], &mut buf, TIMEOUT)
        {
            Ok(_) => {
                let version: u16 = (&buf[0..2]).pread_with(0, BE).unwrap();
                self.hw_version = (version >> HW_VERSION_SHIFT) as u8 & HW_VERSION_MASK;
                self.jtag_version = (version >> JTAG_VERSION_SHIFT) as u8 & JTAG_VERSION_MASK;
            }
            Err(e) => return Err(e),
        }

        // For the STLinkV3 we must use the extended get version command.
        if self.hw_version >= 3 {
            // GET_VERSION_EXT response structure (byte offsets) {
            //  0: HW version
            //  1: SWIM version
            //  2: JTAG/SWD version
            //  3: MSC/VCP version
            //  4: Bridge version
            //  5-7: reserved
            //  8-9: ST_VID
            //  10-11: STLINK_PID
            let mut buf = [0; 12];
            match self
                .device
                .write(vec![commands::GET_VERSION_EXT], &[], &mut buf, TIMEOUT)
            {
                Ok(_) => {
                    let version: u8 = (&buf[2..3]).pread(0).unwrap();
                    self.jtag_version = version;
                }
                Err(e) => return Err(e),
            }
        }

        // Make sure everything is okay with the firmware we use.
        if self.jtag_version == 0 {
            return Err(DebugProbeError::JTAGNotSupportedOnProbe);
        }
        if self.hw_version < 3 && self.jtag_version < Self::MIN_JTAG_VERSION {
            return Err(DebugProbeError::ProbeFirmwareOutdated);
        }

        Ok((self.hw_version, self.jtag_version))
    }

    /// Opens the ST-Link USB device and tries to identify the ST-Links version and it's target voltage.
    /// Internal helper.
    fn init(&mut self) -> Result<(), DebugProbeError> {
        self.enter_idle()?;
        self.get_version()?;
        self.get_target_voltage().map(|_| ())
    }

    /// sets the SWD frequency.
    pub fn set_swd_frequency(
        &mut self,
        frequency: SwdFrequencyToDelayCount,
    ) -> Result<(), DebugProbeError> {
        let mut buf = [0; 2];
        self.device.write(
            vec![
                commands::JTAG_COMMAND,
                commands::SWD_SET_FREQ,
                frequency as u8,
            ],
            &[],
            &mut buf,
            TIMEOUT,
        )?;
        Self::check_status(&buf)
    }

    /// Sets the JTAG frequency.
    pub fn set_jtag_frequency(
        &mut self,
        frequency: JTagFrequencyToDivider,
    ) -> Result<(), DebugProbeError> {
        let mut buf = [0; 2];
        self.device.write(
            vec![
                commands::JTAG_COMMAND,
                commands::JTAG_SET_FREQ,
                frequency as u8,
            ],
            &[],
            &mut buf,
            TIMEOUT,
        )?;
        Self::check_status(&buf)
    }

    pub fn open_ap(&mut self, apsel: impl AccessPort) -> Result<(), DebugProbeError> {
        if self.jtag_version < Self::MIN_JTAG_VERSION_MULTI_AP {
            Err(DebugProbeError::JTagDoesNotSupportMultipleAP)
        } else {
            let mut buf = [0; 2];
            self.device.write(
                vec![
                    commands::JTAG_COMMAND,
                    commands::JTAG_INIT_AP,
                    apsel.get_port_number(),
                    commands::JTAG_AP_NO_CORE,
                ],
                &[],
                &mut buf,
                TIMEOUT,
            )?;
            Self::check_status(&buf)
        }
    }

    pub fn close_ap(&mut self, apsel: impl AccessPort) -> Result<(), DebugProbeError> {
        if self.jtag_version < Self::MIN_JTAG_VERSION_MULTI_AP {
            Err(DebugProbeError::JTagDoesNotSupportMultipleAP)
        } else {
            let mut buf = [0; 2];
            self.device.write(
                vec![
                    commands::JTAG_COMMAND,
                    commands::JTAG_CLOSE_AP_DBG,
                    apsel.get_port_number(),
                ],
                &[],
                &mut buf,
                TIMEOUT,
            )?;
            Self::check_status(&buf)
        }
    }

    /// Drives the nRESET pin.
    /// `is_asserted` tells wheter the reset should be asserted or deasserted.
    pub fn drive_nreset(&mut self, is_asserted: bool) -> Result<(), DebugProbeError> {
        let state = if is_asserted {
            commands::JTAG_DRIVE_NRST_LOW
        } else {
            commands::JTAG_DRIVE_NRST_HIGH
        };
        let mut buf = [0; 2];
        self.device.write(
            vec![commands::JTAG_COMMAND, commands::JTAG_DRIVE_NRST, state],
            &[],
            &mut buf,
            TIMEOUT,
        )?;
        Self::check_status(&buf)
    }

    /// Validates the status given.
    /// Returns an `Err(DebugProbeError::UnknownError)` if the status is not `Status::JtagOk`.
    /// Returns Ok(()) otherwise.
    /// This can be called on any status returned from the attached target.
    fn check_status(status: &[u8]) -> Result<(), DebugProbeError> {
        if status[0] != Status::JtagOk as u8 {
            Err(DebugProbeError::UnknownError)
        } else {
            Ok(())
        }
    }
}
