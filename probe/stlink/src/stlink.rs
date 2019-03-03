use coresight::access_ports::generic_ap::GenericAP;
use coresight::access_ports::AccessPortError;
use memory::ToMemoryReadSize;
use memory::MI;
use memory::memory_interface::ADIMemoryInterface;
use coresight::ap_access::AccessPort;
use coresight::access_ports::APRegister;
use crate::usb_interface::STLinkInfo;
use coresight::access_ports::memory_ap::{MemoryAP};
use coresight::ap_access::APAccess;
use libusb::Device;
use libusb::Error;
use scroll::{Pread, BE};

use coresight::dap_access::DAPAccess;
use probe::debug_probe::{DebugProbe, DebugProbeError};
use probe::protocol::WireProtocol;

use crate::constants::{commands, JTagFrequencyToDivider, Status, SwdFrequencyToDelayCount};
use crate::usb_interface::{STLinkUSBDevice, TIMEOUT};

pub struct STLink {
    device: STLinkUSBDevice,
    hw_version: u8,
    jtag_version: u8,
    protocol: WireProtocol,
    current_apsel: u8,
    current_apbanksel: u8,
}

pub trait ToSTLinkErr<T> {
    fn or_usb_err(self) -> Result<T, DebugProbeError>;
}

impl<T> ToSTLinkErr<T> for libusb::Result<T> {
    fn or_usb_err(self) -> Result<T, DebugProbeError> {
        match self {
            Ok(t) => Ok(t),
            Err(_) => Err(DebugProbeError::USBError),
        }
    }
}

impl DebugProbe for STLink {
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
                    let version: u8 = (&buf[3..4]).pread(0).unwrap();
                    self.jtag_version = version;
                }
                Err(e) => return Err(e),
            }
        }

        // Make sure everything is okay with the firmware we use.
        if self.jtag_version == 0 {
            return Err(DebugProbeError::JTAGNotSupportedOnProbe);
        }
        if self.jtag_version < Self::MIN_JTAG_VERSION {
            return Err(DebugProbeError::ProbeFirmwareOutdated);
        }

        Ok((self.hw_version, self.jtag_version))
    }

    fn get_name(&self) -> &str {
        "ST-Link"
    }

    /// Enters debug mode.
    fn attach(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
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
        Self::check_status(&buf)
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
    type Error = DebugProbeError;

    /// Reads the DAP register on the specified port and address.
    fn read_register(&mut self, port: u16, addr: u16) -> Result<u32, Self::Error> {
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
            Err(DebugProbeError::BlanksNotAllowedOnDPRegister)
        }
    }

    /// Writes a value to the DAP register on the specified port and address.
    fn write_register(&mut self, port: u16, addr: u16, value: u32) -> Result<(), Self::Error> {
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
            Err(DebugProbeError::BlanksNotAllowedOnDPRegister)
        }
    }
}

fn read_register_ap<AP, REGISTER>(link: &mut STLink, port: AP, _register: REGISTER) -> Result<REGISTER, DebugProbeError>
where
    AP: AccessPort,
    REGISTER: APRegister<AP>
{
    use coresight::ap_access::AccessPort;
    // TODO: Make those next lines use the future typed DP interface.
    let cache_changed = if link.current_apsel != port.get_port_number() {
        link.current_apsel = port.get_port_number();
        true
    } else if link.current_apbanksel != REGISTER::APBANKSEL {
        link.current_apbanksel = REGISTER::APBANKSEL;
        true
    } else {
        false
    };
    if cache_changed {
        let select = (u32::from(link.current_apsel) << 24) | (u32::from(link.current_apbanksel) << 4);
        link.write_register(0xFFFF, 0x008, select)?;
    }
    //println!("{:?}, {:08X}", link.current_apsel, REGISTER::ADDRESS);
    let result = link.read_register(u16::from(link.current_apsel), u16::from(REGISTER::ADDRESS))?;
    Ok(REGISTER::from(result))
}

fn write_register_ap<AP, REGISTER>(link: &mut STLink, port: AP, register: REGISTER) -> Result<(), DebugProbeError>
where
    AP: AccessPort,
    REGISTER: APRegister<AP>
{
    use coresight::ap_access::AccessPort;
    // TODO: Make those next lines use the future typed DP interface.
    let cache_changed = if link.current_apsel != port.get_port_number() {
        link.current_apsel = port.get_port_number();
        true
    } else if link.current_apbanksel != REGISTER::APBANKSEL {
        link.current_apbanksel = REGISTER::APBANKSEL;
        true
    } else {
        false
    };
    if cache_changed {
        let select = (u32::from(link.current_apsel) << 24) | (u32::from(link.current_apbanksel) << 4);
        link.write_register(0xFFFF, 0x008, select)?;
    }
    link.write_register(u16::from(link.current_apsel), u16::from(REGISTER::ADDRESS), register.into())?;
    Ok(())
}

impl<REGISTER> APAccess<MemoryAP, REGISTER> for STLink
where
    REGISTER: APRegister<MemoryAP>
{
    type Error = DebugProbeError;

    fn read_register_ap(&mut self, port: MemoryAP, register: REGISTER) -> Result<REGISTER, Self::Error> {
        read_register_ap(self, port, register)
    }
    
    fn write_register_ap(&mut self, port: MemoryAP, register: REGISTER) -> Result<(), Self::Error> {
        write_register_ap(self, port, register)
    }
}

impl<REGISTER> APAccess<GenericAP, REGISTER> for STLink
where
    REGISTER: APRegister<GenericAP>
{
    type Error = DebugProbeError;

    fn read_register_ap(&mut self, port: GenericAP, register: REGISTER) -> Result<REGISTER, Self::Error> {
        read_register_ap(self, port, register)
    }
    
    fn write_register_ap(&mut self, port: GenericAP, register: REGISTER) -> Result<(), Self::Error> {
        write_register_ap(self, port, register)
    }
}

impl Drop for STLink {
    fn drop(&mut self) {
        // We ignore the error case as we can't do much about it anyways.
        let _ = self.enter_idle();
    }
}

impl MI for STLink
{
    fn read<S: ToMemoryReadSize>(&mut self, address: u32) -> Result<S, AccessPortError> {
        ADIMemoryInterface::new(0).read(self, address)
    }

    fn read_block<S: ToMemoryReadSize>(
        &mut self,
        address: u32,
        data: &mut [S]
    ) -> Result<(), AccessPortError> {
        ADIMemoryInterface::new(0).read_block(self, address, data)
    }

    fn write<S: ToMemoryReadSize>(
        &mut self,
        addr: u32,
        data: S
    ) -> Result<(), AccessPortError> {
        ADIMemoryInterface::new(0).write(self, addr, data)
    }

    fn write_block<S: ToMemoryReadSize>(
        &mut self,
        addr: u32,
        data: &[S]
    ) -> Result<(), AccessPortError> {
        ADIMemoryInterface::new(0).write_block(self, addr, data)
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

    /// Port number to use to indicate DP registers.
    const DP_PORT: u16 = 0xffff;

    /// Creates a new STLink device instance.
    /// This function takes care of all the initialization routines and expects a selector closure.
    /// The selector closure is served with a list of connected, eligible ST-Links and should return one of them.
    pub fn new_from_connected<F>(device_selector: F) -> Result<Self, DebugProbeError>
    where
        F: for<'a> FnMut(Vec<(Device<'a>, STLinkInfo)>) -> Result<Device<'a>, Error>,
    {
        let mut stlink = Self {
            device: STLinkUSBDevice::new(device_selector)?,
            hw_version: 0,
            jtag_version: 0,
            protocol: WireProtocol::Swd,
            current_apsel: 0x0000,
            current_apbanksel: 0x00,
        };

        stlink.init()?;

        Ok(stlink)
    }

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
                    apsel.get_port_number()
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
