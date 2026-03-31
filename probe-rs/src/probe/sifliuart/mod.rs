//! SiFli UART Debug Probe, Only support SiFli chip.
//! Refer to <https://webfile.lovemcu.cn/file/user%20manual/UM5201-SF32LB52x-%E7%94%A8%E6%88%B7%E6%89%8B%E5%86%8C%20V0p81.pdf#153> for specific communication formats

mod arm;
/// Shared UART console handle for SiFli UART probes.
pub mod console;
mod transport;

use crate::Error;
use crate::architecture::arm::sequences::ArmDebugSequence;
use crate::architecture::arm::{ArmDebugInterface, ArmError};
use crate::probe::ProbeAuxChannel;
use crate::probe::sifliuart::arm::SifliUartArmDebug;
use crate::probe::sifliuart::console::SifliUartConsole;
use crate::probe::sifliuart::transport::SifliUartTransport;
use crate::probe::{
    DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, ProbeCreationError,
    ProbeFactory, WireProtocol,
};
use serialport::{SerialPort, SerialPortType, available_ports};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{env, fmt};

const START_WORD: [u8; 2] = [0x7E, 0x79];

const DEFUALT_RECV_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_SERIAL_TIMEOUT: Duration = Duration::from_millis(10);

const DEFUALT_UART_BAUD: u32 = 1000000;

#[derive(Debug)]
pub(crate) enum SifliUartCommand<'a> {
    Enter,
    Exit,
    MEMRead { addr: u32, len: u16 },
    MEMWrite { addr: u32, data: &'a [u32] },
}

enum SifliUartResponse {
    Enter,
    Exit,
    MEMRead { data: Vec<u8> },
    MEMWrite,
}

impl fmt::Display for SifliUartCommand<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SifliUartCommand::Enter => write!(f, "Enter"),
            SifliUartCommand::Exit => write!(f, "Exit"),
            SifliUartCommand::MEMRead { addr, len } => {
                write!(f, "MEMRead {{ addr: {addr:#X}, len: {len:#X} }}")
            }
            SifliUartCommand::MEMWrite { addr, data } => {
                write!(f, "MEMWrite {{ addr: {addr:#X}, data: [")?;
                for (i, d) in data.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{d:#X}")?;
                }
                write!(f, "] }}")
            }
        }
    }
}

impl fmt::Display for SifliUartResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SifliUartResponse::Enter => write!(f, "Enter"),
            SifliUartResponse::Exit => write!(f, "Exit"),
            SifliUartResponse::MEMRead { data } => {
                write!(f, "MEMRead {{ data: [")?;
                for (i, byte) in data.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{byte:#04X}")?;
                }
                write!(f, "] }}")
            }
            SifliUartResponse::MEMWrite => write!(f, "MEMWrite"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum CommandError {
    ParameterError(std::io::Error),
    // Error(u64),
    // Unsupported(u64),
    ProbeError(std::io::Error),
    // UnsupportedVersion(u64),
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommandError::ParameterError(e) => write!(f, "ParameterError({e})"),
            // CommandError::Error(e) => write!(f, "Error({})", e),
            // CommandError::Unsupported(e) => write!(f, "Unsupported({})", e),
            CommandError::ProbeError(e) => write!(f, "ProbeError({e})"),
            // CommandError::UnsupportedVersion(e) => write!(f, "UnsupportedVersion({})", e),
        }
    }
}

/// SiFli UART Debug Probe, Only support SiFli chip.
pub struct SifliUart {
    transport: Arc<Mutex<SifliUartTransport>>,
    _serial_port: Box<dyn SerialPort>,
    baud: u32,
}

impl fmt::Debug for SifliUart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SifliUart")
            .field("baud", &self.baud)
            .finish()
    }
}

impl fmt::Display for SifliUart {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Sifli UART Debug Probe")
    }
}

impl SifliUart {
    /// Create a new SiFli UART Debug Probe.
    pub fn new(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        port: Box<dyn SerialPort>,
    ) -> Result<Self, DebugProbeError> {
        let transport = Arc::new(Mutex::new(SifliUartTransport::new(reader, writer)));

        let probe = SifliUart {
            transport,
            baud: DEFUALT_UART_BAUD,
            _serial_port: port,
        };
        Ok(probe)
    }

    fn command(&mut self, command: SifliUartCommand) -> Result<SifliUartResponse, CommandError> {
        tracing::info!("Command: {}", command);
        let mut transport = self.transport.lock().unwrap();
        transport.transaction(&command, DEFUALT_RECV_TIMEOUT)
    }

    fn take_console(&self) -> SifliUartConsole {
        SifliUartConsole::new(self.transport.clone())
    }
}

impl DebugProbe for SifliUart {
    fn get_name(&self) -> &str {
        "Sifli UART Debug Probe"
    }

    fn speed_khz(&self) -> u32 {
        self.baud / 1000
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        self.baud = speed_khz * 1000;

        Ok(speed_khz)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        let ret = self.command(SifliUartCommand::Enter);
        if let Err(e) = ret {
            tracing::error!("Enter command error: {:?}", e);
            return Err(DebugProbeError::NotAttached);
        }
        Ok(())
    }

    fn detach(&mut self) -> Result<(), Error> {
        let ret = self.command(SifliUartCommand::Exit);
        if let Err(e) = ret {
            tracing::error!("Exit command error: {:?}", e);
            return Err(Error::from(DebugProbeError::Other(
                "Exit command error".to_string(),
            )));
        }
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        todo!()
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        todo!()
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        todo!()
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        match protocol {
            WireProtocol::Swd => Ok(()),
            _ => Err(DebugProbeError::UnsupportedProtocol(protocol)),
        }
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        Some(WireProtocol::Swd)
    }

    fn has_arm_interface(&self) -> bool {
        true
    }

    fn try_get_arm_debug_interface<'probe>(
        self: Box<Self>,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Box<dyn ArmDebugInterface + 'probe>, (Box<dyn DebugProbe>, ArmError)> {
        Ok(Box::new(SifliUartArmDebug::new(self, sequence)))
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn take_aux_channels(&mut self) -> Vec<ProbeAuxChannel> {
        vec![ProbeAuxChannel::SifliUartConsole(self.take_console())]
    }
}

/// Factory for creating [`SifliUart`] probes.
#[derive(Debug)]
pub struct SifliUartFactory;

impl SifliUartFactory {
    fn is_sifli_uart(port_type: SerialPortType, port_name: &str) -> Option<DebugProbeInfo> {
        // Only accept /dev/cu.* values on macos, to avoid having two
        // copies of the port (both /dev/tty.* and /dev/cu.*)
        if cfg!(target_os = "macos") && !port_name.contains("/cu.") {
            return None;
        }

        let usb_info = match port_type {
            SerialPortType::UsbPort(info) => info,
            _ => return None,
        };

        if env::var("SIFLI_UART_DEBUG").is_err()
            && (usb_info.product.is_none()
                || !usb_info
                    .product
                    .as_ref()
                    .unwrap()
                    .to_lowercase()
                    .contains("sifli"))
        {
            return None;
        }

        let vendor_id = usb_info.vid;
        let product_id = usb_info.pid;
        let serial_number = Some(port_name.to_string()); //We set serial_number to the serial device number to make it easier to specify the
        let interface = usb_info.interface;
        let identifier = "Sifli uart debug probe".to_string();

        Some(DebugProbeInfo {
            identifier,
            vendor_id,
            product_id,
            serial_number,
            probe_factory: &SifliUartFactory,
            interface,
            is_hid_interface: false,
        })
    }

    fn open_port(&self, port_name: &str) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        let mut port = serialport::new(port_name, DEFUALT_UART_BAUD)
            .dtr_on_open(false)
            .timeout(DEFAULT_SERIAL_TIMEOUT)
            .open()
            .map_err(|_| {
                DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::CouldNotOpen)
            })?;
        port.write_data_terminal_ready(false).map_err(|_| {
            DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::CouldNotOpen)
        })?;
        port.write_request_to_send(false).map_err(|_| {
            DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::CouldNotOpen)
        })?;

        let reader = port.try_clone().map_err(|_| {
            DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::CouldNotOpen)
        })?;
        let writer = reader.try_clone().map_err(|_| {
            DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::CouldNotOpen)
        })?;

        SifliUart::new(Box::new(reader), Box::new(writer), port)
            .map(|probe| Box::new(probe) as Box<dyn DebugProbe>)
    }
}

impl SifliUartResponse {
    fn from_payload(recv_data: Vec<u8>) -> Result<Self, CommandError> {
        let Some(&last) = recv_data.last() else {
            return Err(CommandError::ParameterError(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid frame size",
            )));
        };

        if last != 0x06 {
            return Err(CommandError::ParameterError(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid end of frame",
            )));
        }

        match recv_data[0] {
            0xD1 => Ok(SifliUartResponse::Enter),
            0xD0 => Ok(SifliUartResponse::Exit),
            0xD2 => Ok(SifliUartResponse::MEMRead {
                data: recv_data[1..recv_data.len() - 1].to_vec(),
            }),
            0xD3 => Ok(SifliUartResponse::MEMWrite),
            _ => Err(CommandError::ParameterError(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid frame type",
            ))),
        }
    }
}

impl std::fmt::Display for SifliUartFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SifliUart")
    }
}

impl ProbeFactory for SifliUartFactory {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        let Ok(ports) = available_ports() else {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::CouldNotOpen,
            ));
        };

        if selector.serial_number.is_some() {
            return self.open_port(selector.serial_number.as_ref().unwrap());
        }

        for port in ports {
            let Some(_info) = SifliUartFactory::is_sifli_uart(port.port_type, &port.port_name)
            else {
                continue;
            };

            return self.open_port(&port.port_name);
        }

        Err(DebugProbeError::ProbeCouldNotBeCreated(
            ProbeCreationError::NotFound,
        ))
    }

    fn list_probes(&self) -> Vec<DebugProbeInfo> {
        let mut probes = vec![];
        let Ok(ports) = available_ports() else {
            return probes;
        };
        for port in ports {
            let Some(info) = SifliUartFactory::is_sifli_uart(port.port_type, &port.port_name)
            else {
                continue;
            };
            probes.push(info);
        }
        probes
    }
}
