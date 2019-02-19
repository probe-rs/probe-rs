use coresight::access_ports::memory_ap::MemoryAPValue;
use coresight::access_ports::memory_ap::MemoryAPRegister;
use coresight::access_ports::memory_ap::MemoryAP;
use coresight::ap_access::APAccess;
use scroll::{Pread, BE};

use coresight::dap_access::DAPAccess;
use probe::debug_probe::DebugProbe;
use probe::protocol::WireProtocol;

use crate::constants::{commands, JTagFrequencyToDivider, Status, SwdFrequencyToDelayCount};
use crate::usb_interface::{STLinkUSBDevice, TIMEOUT};

type AccessPort = u8;

pub struct STLink<'a> {
    device: STLinkUSBDevice<'a>,
    hw_version: u8,
    jtag_version: u8,
    protocol: WireProtocol,
}

#[derive(Debug)]
pub enum STLinkError {
    USB(libusb::Error),
    JTAGNotSupportedOnProbe,
    ProbeFirmwareOutdated,
    VoltageDivisionByZero,
    UnknownMode,
    JTagDoesNotSupportMultipleAP,
    UnknownError,
    TransferFault(u32, u16),
    DataAlignmentError,
    Access16BitNotSupported,
    BlanksNotAllowedOnDPRegister,
    RegisterAddressMustBe16Bit,
    NotEnoughBytesRead,
    EndpointNotFound,
}

pub trait ToSTLinkErr<T> {
    fn or_usb_err(self) -> Result<T, STLinkError>;
}

impl<T> ToSTLinkErr<T> for libusb::Result<T> {
    fn or_usb_err(self) -> Result<T, STLinkError> {
        match self {
            Ok(t) => Ok(t),
            Err(e) => Err(STLinkError::USB(e)),
        }
    }
}

impl<'a> DebugProbe for STLink<'a> {
    type Error = STLinkError;

    /// Opens the ST-Link USB device and tries to identify the ST-Links version and it's target voltage.
    fn open(&mut self) -> Result<(), Self::Error> {
        self.device.open()?;
        self.enter_idle()?;
        self.get_version()?;
        self.get_target_voltage().map(|_| ())
    }

    /// Closes the ST-Link USB device.
    fn close(&mut self) -> Result<(), Self::Error> {
        self.enter_idle()?;
        self.device.close().or_else(|e| Err(STLinkError::USB(e)))
    }

    /// Reads the ST-Links version.
    /// Returns a tuple (hardware version, firmware version).
    /// This method stores the version data on the struct to make later use of it.
    fn get_version(&mut self) -> Result<(u8, u8), Self::Error> {
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
                    let version: u8 = (&buf[3..4]).pread(0).unwrap();
                    self.jtag_version = version;
                }
                Err(e) => return Err(e),
            }
        }

        // Make sure everything is okay with the firmware we use.
        if self.jtag_version == 0 {
            return Err(STLinkError::JTAGNotSupportedOnProbe);
        }
        if self.jtag_version < Self::MIN_JTAG_VERSION {
            return Err(STLinkError::ProbeFirmwareOutdated);
        }

        Ok((self.hw_version, self.jtag_version))
    }

    /// Enters debug mode.
    fn attach(&mut self, protocol: WireProtocol) -> Result<(), Self::Error> {
        self.enter_idle()?;

        let param = match protocol {
            WireProtocol::Jtag => commands::JTAG_ENTER_JTAG_NO_CORE_RESET,
            WireProtocol::Swd => commands::JTAG_ENTER_SWD,
        };

        let mut buf = [0; 2];
        self.device.write(
            vec![commands::JTAG_COMMAND, commands::JTAG_ENTER2, param, 0],
            &[],
            &mut buf,
            TIMEOUT,
        )?;
        self.protocol = protocol;
        return Self::check_status(&buf);
    }

    /// Leave debug mode.
    fn detach(&mut self) -> Result<(), Self::Error> {
        self.enter_idle()
    }

    /// Asserts the nRESET pin.
    fn target_reset(&mut self) -> Result<(), Self::Error> {
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
        return Self::check_status(&buf);
    }
}

impl<'a> DAPAccess for STLink<'a> {
    type Error = STLinkError;

    /// Reads the DAP register on the specified port and address.
    fn read_register(&mut self, port: u16, addr: u32) -> Result<u32, Self::Error> {
        if (addr & 0xf0) == 0 || port != Self::DP_PORT {
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
            Err(STLinkError::BlanksNotAllowedOnDPRegister)
        }
    }

    /// Writes a value to the DAP register on the specified port and address.
    fn write_register(&mut self, port: u16, addr: u32, value: u32) -> Result<(), Self::Error> {
        if (addr & 0xf0) == 0 || port != Self::DP_PORT {
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
            Err(STLinkError::BlanksNotAllowedOnDPRegister)
        }
    }
}

impl<'a> APAccess<MemoryAP, MemoryAPRegister, MemoryAPValue> for STLink<'a> {
    fn read_register(port: MemoryAP, register: MemoryAPRegister) -> MemoryAPValue {
        MemoryAPValue
    }
    
    fn write_register(port: MemoryAP, register: MemoryAPRegister, value: MemoryAPValue) {

    }
}

impl<'a> STLink<'a> {
    /// Maximum number of bytes to send or receive for 32- and 16- bit transfers.
    ///
    /// 8-bit transfers have a maximum size of the maximum USB packet size (64 bytes for full speed).
    const MAXIMUM_TRANSFER_SIZE: u32 = 1024;

    /// Minimum required STLink firmware version.
    const MIN_JTAG_VERSION: u8 = 24;

    /// Firmware version that adds 16-bit transfers.
    const MIN_JTAG_VERSION_16BIT_XFER: u8 = 26;

    /// Firmware version that adds multiple AP support.
    const MIN_JTAG_VERSION_MULTI_AP: u8 = 28;

    /// Port number to use to indicate DP registers.
    const DP_PORT: u16 = 0xffff;

    pub fn new(device: STLinkUSBDevice<'a>) -> Self {
        Self {
            device,
            hw_version: 0,
            jtag_version: 0,
            protocol: WireProtocol::Swd,
        }
    }

    /// Reads the target voltage.
    /// For the china fake variants this will always read a nonzero value!
    pub fn get_target_voltage(&mut self) -> Result<f32, STLinkError> {
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
                    Err(STLinkError::VoltageDivisionByZero)
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Commands the ST-Link to enter idle mode.
    fn enter_idle(&mut self) -> Result<(), STLinkError> {
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
                    // Err(STLinkError::UnknownMode)
                }
            }
            Err(e) => Err(e),
        }
    }

    /// sets the SWD frequency.
    pub fn set_swd_frequency(
        &mut self,
        frequency: SwdFrequencyToDelayCount,
    ) -> Result<(), STLinkError> {
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
    ) -> Result<(), STLinkError> {
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

    pub fn open_ap(&mut self, apsel: AccessPort) -> Result<(), STLinkError> {
        if self.jtag_version < Self::MIN_JTAG_VERSION_MULTI_AP {
            return Err(STLinkError::JTagDoesNotSupportMultipleAP);
        }
        let mut buf = [0; 2];
        self.device.write(
            vec![
                commands::JTAG_COMMAND,
                commands::JTAG_INIT_AP,
                apsel,
                commands::JTAG_AP_NO_CORE,
            ],
            &[],
            &mut buf,
            TIMEOUT,
        )?;
        return Self::check_status(&buf);
    }

    pub fn close_ap(&mut self, apsel: AccessPort) -> Result<(), STLinkError> {
        if self.jtag_version < Self::MIN_JTAG_VERSION_MULTI_AP {
            return Err(STLinkError::JTagDoesNotSupportMultipleAP);
        }
        let mut buf = [0; 2];
        self.device.write(
            vec![commands::JTAG_COMMAND, commands::JTAG_CLOSE_AP_DBG, apsel],
            &[],
            &mut buf,
            TIMEOUT,
        )?;
        return Self::check_status(&buf);
    }

    /// Drives the nRESET pin.
    /// `is_asserted` tells wheter the reset should be asserted or deasserted.
    pub fn drive_nreset(&mut self, is_asserted: bool) -> Result<(), STLinkError> {
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
        return Self::check_status(&buf);
    }

    /// Validates the status given.
    /// Returns an `Err(STLinkError::UnknownError)` if the status is not `Status::JtagOk`.
    /// Returns Ok(()) otherwise.
    /// This can be called on any status returned from the attached target.
    fn check_status(status: &[u8]) -> Result<(), STLinkError> {
        if status[0] != Status::JtagOk as u8 {
            Err(STLinkError::UnknownError)
        } else {
            Ok(())
        }
    }

    pub fn clear_sticky_error(&mut self) -> Result<(), STLinkError> {
        // TODO: Implement this as soon as CoreSight is implemented
        // match self.protocol {
        //     WireProtocol::Jtag => self.write_dap_register(Self::DP_PORT, dap.DP_CTRL_STAT, dap.CTRLSTAT_STICKYERR),
        //     WireProtocol::Swd => self.write_dap_register(Self::DP_PORT, dap.DP_ABORT, dap.ABORT_STKERRCLR)
        // }
        Ok(())
    }

    pub fn read_mem(
        &mut self,
        mut addr: u32,
        mut size: u32,
        memcmd: u8,
        max: u32,
        apsel: AccessPort,
    ) -> Result<Vec<u8>, STLinkError> {
        let mut result = vec![];
        while size > 0 {
            let transfer_size = u32::min(size, max);

            let cmd = vec![
                commands::JTAG_COMMAND,
                memcmd,
                addr as u8,
                (addr >> 8) as u8,
                (addr >> 16) as u8,
                (addr >> 24) as u8,
                (transfer_size >> 0) as u8,
                (transfer_size >> 8) as u8,
                apsel,
            ];
            let mut buf = Vec::with_capacity(transfer_size as usize);
            self.device.write(cmd, &[], buf.as_mut_slice(), TIMEOUT)?;
            result.extend(buf.into_iter());

            addr += transfer_size as u32;
            size -= transfer_size;

            // Check status of this read.
            let mut buf = [0; 12];
            self.device.write(
                vec![commands::JTAG_COMMAND, commands::JTAG_GETLASTRWSTATUS2],
                &[],
                &mut buf,
                TIMEOUT,
            )?;
            let status: u16 = (&buf[0..2]).pread(0).unwrap();
            let fault_address: u32 = (&buf[4..8]).pread(0).unwrap();
            if if status == Status::JtagUnknownError as u16 {
                true
            } else if status == Status::SwdApFault as u16 {
                true
            } else if status == Status::SwdDpFault as u16 {
                true
            } else if status == Status::JtagOk as u16 {
                return Err(STLinkError::UnknownError);
            } else {
                false
            } {
                self.clear_sticky_error().ok();
                return Err(STLinkError::TransferFault(
                    fault_address,
                    (transfer_size - (fault_address - addr)) as u16,
                ));
            }
        }
        Ok(result)
    }

    pub fn write_mem(
        &mut self,
        mut addr: u32,
        mut data: Vec<u8>,
        memcmd: u8,
        max: u32,
        apsel: AccessPort,
    ) -> Result<(), STLinkError> {
        while data.len() > 0 {
            let transfer_size = u32::min(data.len() as u32, max);
            let transfer_data = &data[0..transfer_size as usize];

            let cmd = vec![
                commands::JTAG_COMMAND,
                memcmd,
                addr as u8,
                (addr >> 8) as u8,
                (addr >> 16) as u8,
                (addr >> 24) as u8,
                (transfer_size >> 0) as u8,
                (transfer_size >> 8) as u8,
                apsel,
            ];
            self.device.write(cmd, transfer_data, &mut [], TIMEOUT)?;

            addr += transfer_size as u32;
            data.drain(..transfer_size as usize);

            // Check status of this read.
            let mut buf = [0; 12];
            self.device.write(
                vec![commands::JTAG_COMMAND, commands::JTAG_GETLASTRWSTATUS2],
                &[],
                &mut buf,
                TIMEOUT,
            )?;
            let status: u16 = (&buf[0..2]).pread(0).unwrap();
            let fault_address: u32 = (&buf[4..8]).pread(0).unwrap();
            if if status == Status::JtagUnknownError as u16 {
                true
            } else if status == Status::SwdApFault as u16 {
                true
            } else if status == Status::SwdDpFault as u16 {
                true
            } else if status == Status::JtagOk as u16 {
                return Err(STLinkError::UnknownError);
            } else {
                false
            } {
                self.clear_sticky_error().ok();
                return Err(STLinkError::TransferFault(
                    fault_address,
                    (transfer_size - (fault_address - addr)) as u16,
                ));
            }
        }
        Ok(())
    }

    pub fn read_mem32(
        &mut self,
        addr: u32,
        size: u32,
        apsel: AccessPort,
    ) -> Result<Vec<u8>, STLinkError> {
        if (addr & 0x3) == 0 && (size & 0x3) == 0 {
            return self.read_mem(
                addr,
                size,
                commands::JTAG_READMEM_32BIT,
                Self::MAXIMUM_TRANSFER_SIZE,
                apsel,
            );
        }
        Err(STLinkError::DataAlignmentError)
    }

    pub fn write_mem32(
        &mut self,
        addr: u32,
        data: Vec<u8>,
        apsel: AccessPort,
    ) -> Result<(), STLinkError> {
        if (addr & 0x3) == 0 && (data.len() & 0x3) == 0 {
            return self.write_mem(
                addr,
                data,
                commands::JTAG_WRITEMEM_32BIT,
                Self::MAXIMUM_TRANSFER_SIZE,
                apsel,
            );
        }
        Err(STLinkError::DataAlignmentError)
    }

    pub fn read_mem16(
        &mut self,
        addr: u32,
        size: u32,
        apsel: AccessPort,
    ) -> Result<Vec<u8>, STLinkError> {
        if (addr & 0x1) == 0 && (size & 0x1) == 0 {
            if self.jtag_version < Self::MIN_JTAG_VERSION_16BIT_XFER {
                return Err(STLinkError::Access16BitNotSupported);
            }
            return self.read_mem(
                addr,
                size,
                commands::JTAG_READMEM_16BIT,
                Self::MAXIMUM_TRANSFER_SIZE,
                apsel,
            );
        }
        Err(STLinkError::DataAlignmentError)
    }

    pub fn write_mem16(
        &mut self,
        addr: u32,
        data: Vec<u8>,
        apsel: AccessPort,
    ) -> Result<(), STLinkError> {
        if (addr & 0x1) == 0 && (data.len() & 0x1) == 0 {
            if self.jtag_version < Self::MIN_JTAG_VERSION_16BIT_XFER {
                return Err(STLinkError::Access16BitNotSupported);
            }
            return self.write_mem(
                addr,
                data,
                commands::JTAG_WRITEMEM_16BIT,
                Self::MAXIMUM_TRANSFER_SIZE,
                apsel,
            );
        }
        Err(STLinkError::DataAlignmentError)
    }

    pub fn read_mem8(
        &mut self,
        addr: u32,
        size: u32,
        apsel: AccessPort,
    ) -> Result<Vec<u8>, STLinkError> {
        self.read_mem(
            addr,
            size,
            commands::JTAG_READMEM_8BIT,
            Self::MAXIMUM_TRANSFER_SIZE,
            apsel,
        )
    }

    pub fn write_mem8(
        &mut self,
        addr: u32,
        data: Vec<u8>,
        apsel: AccessPort,
    ) -> Result<(), STLinkError> {
        self.write_mem(
            addr,
            data,
            commands::JTAG_WRITEMEM_8BIT,
            Self::MAXIMUM_TRANSFER_SIZE,
            apsel,
        )
    }
}
