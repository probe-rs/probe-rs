use coresight::access_ports::generic_ap::GenericAP;
use coresight::access_ports::AccessPortError;
use memory::ToMemoryReadSize;
use memory::MI;
use coresight::ap_access::AccessPort;
use coresight::access_ports::APRegister;
use coresight::access_ports::memory_ap::{MemoryAP};
use coresight::ap_access::APAccess;
use libusb::Device;
use libusb::Error;
use scroll::{Pread, BE};

use memory::adi_v5_memory_interface::ADIMemoryInterface;

use coresight::dap_access::DAPAccess;
use probe::debug_probe::{DebugProbe, DebugProbeError};
use probe::protocol::WireProtocol;

use crate::commands::{
    Status,
    general::{
        connect::{
            ConnectRequest,
            ConnectResponse,
        },
        disconnect::{
            DisconnectRequest,
            DisconnectResponse,
        },
        reset::{
            ResetRequest,
            ResetResponse,
        }
    },
    transfer::{
        transfer::{
            TransferRequest,
            TransferResponse,
            InnerTransferRequest,
            InnerTransferResponse,
            Ack,
            Port,
            RW,
        }
    },
};

pub struct DAPLink {
    pub device: hidapi::HidDeviceInfo,
    hw_version: u8,
    jtag_version: u8,
    protocol: WireProtocol,
    current_apsel: u8,
    current_apbanksel: u8,
}

impl DAPLink {
    const DP_PORT: u16 = 0xffff;

    pub fn new_from_device(device: hidapi::HidDeviceInfo) -> Self {
        Self {
            device,
            hw_version: 0,
            jtag_version: 0,
            protocol: WireProtocol::Swd,
            current_apsel: 0,
            current_apbanksel: 0,
        }
    }
}

impl DebugProbe for DAPLink {
    /// Reads the ST-Links version.
    /// Returns a tuple (hardware version, firmware version).
    /// This method stores the version data on the struct to make later use of it.
    fn get_version(&mut self) -> Result<(u8, u8), DebugProbeError> {
        Ok((42, 42))
    }

    fn get_name(&self) -> &str {
        "ST-Link"
    }

    /// Enters debug mode.
    fn attach(&mut self, protocol: Option<WireProtocol>) -> Result<WireProtocol, DebugProbeError> {
        use crate::commands::Error;
        crate::commands::send_command(
            &self.device,
            if let Some(protocol) = protocol {
                match protocol {
                    WireProtocol::Swd => ConnectRequest::UseSWD,
                    WireProtocol::Jtag => ConnectRequest::UseJTAG,
                }
            } else {
                ConnectRequest::UseDefaultPort
            },
        )
        .map_err(|e| match e {
            Error::NotEnoughSpace => DebugProbeError::UnknownError,
            Error::USB => DebugProbeError::USBError,
            Error::UnexpectedAnswer => DebugProbeError::UnknownError,
            Error::DAPError => DebugProbeError::UnknownError,
        })
        .and_then(|v| match v {
            ConnectResponse::SuccessfulInitForSWD => Ok(WireProtocol::Swd),
            ConnectResponse::SuccessfulInitForJTAG => Ok(WireProtocol::Jtag),
            ConnectResponse::InitFailed => Err(DebugProbeError::UnknownError),
        })
    }

    /// Leave debug mode.
    fn detach(&mut self) -> Result<(), DebugProbeError> {
        crate::commands::send_command(
            &self.device,
            DisconnectRequest {},
        )
        .map_err(|e| DebugProbeError::USBError)
        .and_then(|v: DisconnectResponse| match v {
            DisconnectResponse(Status::DAPOk) => Ok(()),
            DisconnectResponse(Status::DAPError) => Err(DebugProbeError::UnknownError)
        })
    }

    /// Asserts the nRESET pin.
    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        use crate::commands::Error;
        crate::commands::send_command(
            &self.device,
            ResetRequest
        )
        .map_err(|e| match e {
            Error::NotEnoughSpace => DebugProbeError::UnknownError,
            Error::USB => DebugProbeError::USBError,
            Error::UnexpectedAnswer => DebugProbeError::UnknownError,
            Error::DAPError => DebugProbeError::UnknownError,
        })
        .map(|v: ResetResponse| {
            println!("{:?}", v);
            ()
        })
    }
}

impl DAPAccess for DAPLink {
    type Error = DebugProbeError;

    /// Reads the DAP register on the specified port and address.
    fn read_register(&mut self, port: u16, addr: u16) -> Result<u32, Self::Error> {
        crate::commands::send_command::<TransferRequest, TransferResponse>(
            &self.device,
            TransferRequest::new(
                InnerTransferRequest::new(Port::AP, RW::R, addr as u8),
                0
            )
        )
        .map_err(|e| DebugProbeError::UnknownError)
        .and_then(|v| {
            if v.transfer_count == 1 {
                if v.transfer_response.protocol_error {
                    Err(DebugProbeError::USBError)
                } else {
                    match v.transfer_response.ack {
                        Ack::OK => Ok(v.transfer_data),
                        _ => Err(DebugProbeError::UnknownError)
                    }
                }
            } else {
                Err(DebugProbeError::UnknownError)
            }
        })
    }

    /// Writes a value to the DAP register on the specified port and address.
    fn write_register(&mut self, port: u16, addr: u16, value: u32) -> Result<(), Self::Error> {
        dbg!(addr);
        dbg!(value);
        crate::commands::send_command::<TransferRequest, TransferResponse>(
            &self.device,
            dbg!(TransferRequest::new(
                InnerTransferRequest::new(Port::AP, RW::W, addr as u8),
                value
            ))
        )
        .map_err(|e| DebugProbeError::UnknownError)
        .and_then(|v| {
            if v.transfer_count == 1 {
                if v.transfer_response.protocol_error {
                    Err(DebugProbeError::USBError)
                } else {
                    match v.transfer_response.ack {
                        Ack::OK => Ok(()),
                        _ => Err(DebugProbeError::UnknownError)
                    }
                }
            } else {
                Err(DebugProbeError::UnknownError)
            }
        })
    }
}

fn read_register_ap<AP, REGISTER>(link: &mut DAPLink, port: AP, _register: REGISTER) -> Result<REGISTER, DebugProbeError>
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

fn write_register_ap<AP, REGISTER>(link: &mut DAPLink, port: AP, register: REGISTER) -> Result<(), DebugProbeError>
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

impl<REGISTER> APAccess<MemoryAP, REGISTER> for DAPLink
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

// impl<REGISTER> APAccess<GenericAP, REGISTER> for DAPLink
// where
//     REGISTER: APRegister<GenericAP>
// {
//     type Error = DebugProbeError;

//     fn read_register_ap(&mut self, port: GenericAP, register: REGISTER) -> Result<REGISTER, Self::Error> {
//         read_register_ap(self, port, register)
//     }
    
//     fn write_register_ap(&mut self, port: GenericAP, register: REGISTER) -> Result<(), Self::Error> {
//         write_register_ap(self, port, register)
//     }
// }

impl Drop for DAPLink {
    fn drop(&mut self) {
        // We ignore the error case as we can't do much about it anyways.
        let _ = self.detach();
    }
}

impl MI for DAPLink
{
    fn read<S: ToMemoryReadSize>(&mut self, address: u32) -> Result<S, AccessPortError> {
        ADIMemoryInterface::new(0).read(self, address)
    }

    fn read_block<S: ToMemoryReadSize>(
        &mut self,
        address: u32,
        data: &mut [S]
    ) -> Result<(), AccessPortError> {
        data[0] = ADIMemoryInterface::new(0).read(self, address)?;
        Ok(())
    }

    fn write<S: ToMemoryReadSize>(
        &mut self,
        addr: u32,
        data: S
    ) -> Result<(), AccessPortError> {
        // ADIMemoryInterface::new(0).write(self, addr, data)
        Err(AccessPortError::ProbeError)
    }

    fn write_block<S: ToMemoryReadSize>(
        &mut self,
        addr: u32,
        data: &[S]
    ) -> Result<(), AccessPortError> {
        // ADIMemoryInterface::new(0).write_block(self, addr, data)
        Err(AccessPortError::ProbeError)
    }
}