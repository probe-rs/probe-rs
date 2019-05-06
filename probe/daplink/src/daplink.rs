use probe::debug_probe::DebugProbeInfo;
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
        },
        configure::{
            ConfigureRequest,
            ConfigureResponse,
        }
    },
    swj::{
        clock::{
            SWJClockRequest,
            SWJClockResponse,
        },
        sequence::{
            SequenceRequest,
            SequenceResponse,
        },
    },
    swd,
};

pub struct DAPLink {
    pub device: hidapi::HidDevice,
    hw_version: u8,
    jtag_version: u8,
    protocol: WireProtocol,
    current_apsel: u8,
    current_apbanksel: u8,
}

impl DAPLink {
    const DP_PORT: u16 = 0xffff;

    pub fn new_from_device(device: hidapi::HidDevice) -> Self {
        Self {
            device,
            hw_version: 0,
            jtag_version: 0,
            protocol: WireProtocol::Swd,
            current_apsel: 0,
            current_apbanksel: 0,
        }
    }

    fn set_swj_clock(&self, clock: u32) -> Result<(), DebugProbeError> {
        use crate::commands::Error;
        crate::commands::send_command::<SWJClockRequest, SWJClockResponse>(
            &self.device,
            SWJClockRequest(clock),
        )
        .map_err(|e| match e {
            Error::NotEnoughSpace => DebugProbeError::UnknownError,
            Error::USB => DebugProbeError::USBError,
            Error::UnexpectedAnswer => DebugProbeError::UnknownError,
            Error::DAPError => DebugProbeError::UnknownError,
            Error::TooMuchData => DebugProbeError::UnknownError,
        })
        .and_then(|v| match v {
            SWJClockResponse(Status::DAPOk) => Ok(()),
            SWJClockResponse(Status::DAPError) => Err(DebugProbeError::UnknownError),
        })
    }

    fn transfer_configure(&self, request: ConfigureRequest) -> Result<(), DebugProbeError> {
        use crate::commands::Error;
        crate::commands::send_command::<ConfigureRequest, ConfigureResponse>(
            &self.device,
            request,
        )
        .map_err(|e| match e {
            Error::NotEnoughSpace => DebugProbeError::UnknownError,
            Error::USB => DebugProbeError::USBError,
            Error::UnexpectedAnswer => DebugProbeError::UnknownError,
            Error::DAPError => DebugProbeError::UnknownError,
            Error::TooMuchData => DebugProbeError::UnknownError,
        })
        .and_then(|v| match v {
            ConfigureResponse(Status::DAPOk) => Ok(()),
            ConfigureResponse(Status::DAPError) => Err(DebugProbeError::UnknownError),
        })
    }

    fn configure_swd(&self, request: swd::configure::ConfigureRequest) -> Result<(), DebugProbeError> {
        use crate::commands::Error;


        crate::commands::send_command::<swd::configure::ConfigureRequest, swd::configure::ConfigureResponse>(
            &self.device, 
            request
        )
        .map_err(|e| match e {
            Error::NotEnoughSpace => DebugProbeError::UnknownError,
            Error::USB => DebugProbeError::USBError,
            Error::UnexpectedAnswer => DebugProbeError::UnknownError,
            Error::DAPError => DebugProbeError::UnknownError,
            Error::TooMuchData => DebugProbeError::UnknownError,
        })
        .and_then(|v| match v {
            swd::configure::ConfigureResponse(Status::DAPOk) => Ok(()),
            swd::configure::ConfigureResponse(Status::DAPError) => Err(DebugProbeError::UnknownError),
        })


    }

    fn send_swj_sequences(&self, request: SequenceRequest) -> Result<(), DebugProbeError> {
        /* 12 38 FF FF FF FF FF FF FF -> 12 00 // SWJ Sequence
        12 10 9E E7 -> 12 00 // SWJ Sequence
        12 38 FF FF FF FF FF FF FF -> 12 00 // SWJ Sequence */
        //let sequence_1 = SequenceRequest::new(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        use crate::commands::Error;


        crate::commands::send_command::<SequenceRequest, SequenceResponse>(
            &self.device, 
            request
        )
        .map_err(|e| match e {
            Error::NotEnoughSpace => DebugProbeError::UnknownError,
            Error::USB => DebugProbeError::USBError,
            Error::UnexpectedAnswer => DebugProbeError::UnknownError,
            Error::DAPError => DebugProbeError::UnknownError,
            Error::TooMuchData => DebugProbeError::UnknownError,
        })
        .and_then(|v| match v {
            SequenceResponse(Status::DAPOk) => Ok(()),
            SequenceResponse(Status::DAPError) => Err(DebugProbeError::UnknownError),
        })

    }
}

impl DebugProbe for DAPLink {
    fn new_from_probe_info(info: DebugProbeInfo) -> Result<Self, DebugProbeError> {
        if let Some(serial_number) = info.serial_number {
            Ok(Self::new_from_device(
                hidapi::HidApi::new()
                    .map_err(|e| DebugProbeError::ProbeCouldNotBeCreated)?
                    .open_serial(info.vendor_id, info.vendor_id, &serial_number)
                    .map_err(|e| DebugProbeError::ProbeCouldNotBeCreated)?
            ))
        } else {
            Ok(Self::new_from_device(
                hidapi::HidApi::new()
                    .map_err(|e| DebugProbeError::ProbeCouldNotBeCreated)?
                    .open(info.vendor_id, info.vendor_id)
                    .map_err(|e| DebugProbeError::ProbeCouldNotBeCreated)?
            ))
        }
    }

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

        self.set_swj_clock(1_000_000)?;

        let result = crate::commands::send_command(
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
            Error::TooMuchData => DebugProbeError::UnknownError,
        })
        .and_then(|v| match v {
            ConnectResponse::SuccessfulInitForSWD => Ok(WireProtocol::Swd),
            ConnectResponse::SuccessfulInitForJTAG => Ok(WireProtocol::Jtag),
            ConnectResponse::InitFailed => Err(DebugProbeError::UnknownError),
        });

        self.set_swj_clock(1_000_000)?;

        self.transfer_configure(ConfigureRequest {
            idle_cycles: 0,
            wait_retry: 80,
            match_retry: 0,
        })?;

        self.configure_swd(swd::configure::ConfigureRequest {})?;

        self.send_swj_sequences(SequenceRequest::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).unwrap())?;

        self.send_swj_sequences(SequenceRequest::new(&[0x9e, 0xe7]).unwrap())?;

        self.send_swj_sequences(SequenceRequest::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).unwrap())?;

        self.send_swj_sequences(SequenceRequest::new(&[0x00]).unwrap())?;



        dbg!(self.read_register(0, 0));

        self.write_register(0, 0x0, 0x1e); // clear errors 

        println!("Writing to Select register (address 8)");
        self.write_register(0, 0x8, 0x0); // select DBPANK 0
        self.write_register(0, 0x4, 0x50_00_00_00); // CSYSPWRUPREQ, CDBGPWRUPREQ

        // TODO: Check return value if power up was ok
        dbg!(self.read_register(0, 0x4)); 

        let ap = GenericAP::new(0);
        use coresight::access_ports::generic_ap::{
            IDR};



        dbg!(read_register_ap(self, ap, IDR::default()));

/* 12 38 FF FF FF FF FF FF FF -> 12 00 // SWJ Sequence
12 10 9E E7 -> 12 00 // SWJ Sequence
12 38 FF FF FF FF FF FF FF -> 12 00 // SWJ Sequence
12 08 00 -> 12 00 // SWJ Sequence */

        result
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
            Error::TooMuchData => DebugProbeError::UnknownError,
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
                InnerTransferRequest::new(Port::DP, RW::R, addr as u8),
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

    /// Reads the DAP register on the specified port and address.
    fn read_register_ap_tmp(&mut self, port: u16, addr: u16) -> Result<u32, Self::Error> {
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
        crate::commands::send_command::<TransferRequest, TransferResponse>(
            &self.device,
            TransferRequest::new(
                InnerTransferRequest::new(Port::DP, RW::W, addr as u8),
                value
            )
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

    /// Writes a value to the DAP register on the specified port and address.
    fn write_register_ap_tmp(&mut self, port: u16, addr: u16, value: u32) -> Result<(), Self::Error> {
        crate::commands::send_command::<TransferRequest, TransferResponse>(
            &self.device,
            TransferRequest::new(
                InnerTransferRequest::new(Port::DP, RW::W, addr as u8),
                value
            )
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
    println!("Reading register {}", REGISTER::NAME);

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
    let result = link.read_register_ap_tmp(u16::from(link.current_apsel), u16::from(REGISTER::ADDRESS))?;
    Ok(REGISTER::from(result))
}

fn write_register_ap<AP, REGISTER>(link: &mut DAPLink, port: AP, register: REGISTER) -> Result<(), DebugProbeError>
where
    AP: AccessPort,
    REGISTER: APRegister<AP>
{
    println!("Write register {}", REGISTER::NAME);

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
    link.write_register_ap_tmp(u16::from(link.current_apsel), u16::from(REGISTER::ADDRESS), register.into())?;
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