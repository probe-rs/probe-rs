//! SiFli UART Debug Probe, Only support SiFli chip.
//! Refer to <https://webfile.lovemcu.cn/file/user%20manual/UM5201-SF32LB52x-%E7%94%A8%E6%88%B7%E6%89%8B%E5%86%8C%20V0p81.pdf#153> for specific communication formats

mod arm;

use crate::Error;
use crate::architecture::arm::communication_interface::UninitializedArmProbe;
use crate::probe::sifliuart::arm::UninitializedSifliUartArmProbe;
use crate::probe::{
    DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, ProbeCreationError,
    ProbeFactory, WireProtocol,
};
use itertools::Itertools;
use probe_rs_target::ScanChainElement;
use serialport::{SerialPort, SerialPortType, available_ports};
use std::io::{BufReader, BufWriter, Read, Write};
use std::time::{Duration, Instant};
use std::{env, fmt};

const START_WORD: [u8; 2] = [0x7E, 0x79];

const DEFUALT_RECV_TIMEOUT: Duration = Duration::from_secs(3);

const DEFUALT_UART_BAUD: u32 = 1000000;

#[allow(dead_code)]
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

impl<'a> fmt::Display for SifliUartCommand<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SifliUartCommand::Enter => write!(f, "Enter"),
            SifliUartCommand::Exit => write!(f, "Exit"),
            SifliUartCommand::MEMRead { addr, len } => {
                write!(f, "MEMRead {{ addr: {:#X}, len: {:#X} }}", addr, len)
            }
            SifliUartCommand::MEMWrite { addr, data } => {
                write!(f, "MEMWrite {{ addr: {:#X}, data: [", addr)?;
                for (i, d) in data.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{:#X}", d)?;
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
                    write!(f, "{:#04X}", byte)?;
                }
                write!(f, "] }}")
            }
            SifliUartResponse::MEMWrite => write!(f, "MEMWrite"),
        }
    }
}

#[allow(dead_code)]
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
            CommandError::ParameterError(e) => write!(f, "ParameterError({})", e),
            // CommandError::Error(e) => write!(f, "Error({})", e),
            // CommandError::Unsupported(e) => write!(f, "Unsupported({})", e),
            CommandError::ProbeError(e) => write!(f, "ProbeError({})", e),
            // CommandError::UnsupportedVersion(e) => write!(f, "UnsupportedVersion({})", e),
        }
    }
}

/// SiFli UART Debug Probe, Only support SiFli chip.
pub struct SifliUart {
    reader: BufReader<Box<dyn Read + Send>>,
    writer: BufWriter<Box<dyn Write + Send>>,
    serial_port: Box<dyn SerialPort>,
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
        let reader = BufReader::new(reader);
        let writer = BufWriter::new(writer);

        let probe = SifliUart {
            reader,
            writer,
            baud: DEFUALT_UART_BAUD,
            serial_port: port,
        };
        Ok(probe)
    }

    fn create_header(len: u16) -> Vec<u8> {
        let mut header = vec![];
        header.extend_from_slice(&START_WORD);
        header.extend_from_slice(&len.to_le_bytes());
        header.push(0x10);
        header.push(0x00);
        header
    }

    fn send(
        writer: &mut BufWriter<Box<dyn Write + Send>>,
        command: &SifliUartCommand,
    ) -> Result<(), CommandError> {
        let mut send_data = vec![];
        match command {
            SifliUartCommand::Enter => {
                let temp = [0x41, 0x54, 0x53, 0x46, 0x33, 0x32, 0x05, 0x21];
                send_data.extend_from_slice(&temp);
            }
            SifliUartCommand::Exit => {
                let temp = [0x41, 0x54, 0x53, 0x46, 0x33, 0x32, 0x18, 0x21];
                send_data.extend_from_slice(&temp);
            }
            SifliUartCommand::MEMRead { addr, len } => {
                send_data.push(0x40);
                send_data.push(0x72);
                send_data.extend_from_slice(&addr.to_le_bytes());
                send_data.extend_from_slice(&len.to_le_bytes());
            }
            SifliUartCommand::MEMWrite { addr, data } => {
                send_data.push(0x40);
                send_data.push(0x77);
                send_data.extend_from_slice(&addr.to_le_bytes());
                send_data.extend_from_slice(&(data.len() as u16).to_le_bytes());
                for d in data.iter() {
                    send_data.extend_from_slice(&d.to_le_bytes());
                }
            }
        }

        let header = Self::create_header(send_data.len() as u16);
        writer
            .write_all(&header)
            .map_err(CommandError::ProbeError)?;
        // tracing::info!("Send data: {:?}", send_data);
        writer
            .write_all(&send_data)
            .map_err(CommandError::ProbeError)?;
        writer.flush().map_err(CommandError::ProbeError)?;

        Ok(())
    }

    fn recv(
        reader: &mut BufReader<Box<dyn Read + Send>>,
    ) -> Result<SifliUartResponse, CommandError> {
        let start_time = Instant::now();
        let mut buffer = vec![];
        let mut recv_data = vec![];

        loop {
            if start_time.elapsed() >= DEFUALT_RECV_TIMEOUT {
                return Err(CommandError::ParameterError(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Timeout",
                )));
            }

            let mut byte = [0; 1];
            if reader.read_exact(&mut byte).is_err() {
                continue;
            }

            if (byte[0] == START_WORD[0]) || (buffer.len() == 1 && byte[0] == START_WORD[1]) {
                buffer.push(byte[0]);
            } else {
                buffer.clear();
            }
            tracing::info!("Recv buffer: {:?}", buffer);

            if buffer.ends_with(&START_WORD) {
                let err = Err(CommandError::ParameterError(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Invalid frame size",
                )));
                recv_data.clear();
                // Header Length
                let mut temp = [0; 2];
                if reader.read_exact(&mut temp).is_err() {
                    return err;
                }
                let size = u16::from_le_bytes(temp);
                tracing::info!("Recv size: {}", size);

                // Header channel and crc
                if reader.read_exact(&mut temp).is_err() {
                    return err;
                }

                while recv_data.len() < size as usize {
                    if reader.read_exact(&mut byte).is_err() {
                        return err;
                    }
                    recv_data.push(byte[0]);
                    tracing::info!("Recv data: {:?}", recv_data);
                }
                break;
            } else if buffer.len() == 2 {
                buffer.clear();
            }
        }

        if recv_data[recv_data.len() - 1] != 0x06 {
            return Err(CommandError::ParameterError(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid end of frame",
            )));
        }

        match recv_data[0] {
            0xD1 => Ok(SifliUartResponse::Enter),
            0xD0 => Ok(SifliUartResponse::Exit),
            0xD2 => {
                let data = recv_data[1..recv_data.len() - 1].to_vec();
                Ok(SifliUartResponse::MEMRead { data })
            }
            0xD3 => Ok(SifliUartResponse::MEMWrite),
            _ => Err(CommandError::ParameterError(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid frame type",
            ))),
        }
    }

    fn command(&mut self, command: SifliUartCommand) -> Result<SifliUartResponse, CommandError> {
        tracing::info!("Command: {}", command);
        let ret = Self::send(&mut self.writer, &command);
        if let Err(e) = ret {
            tracing::error!("Command send error: {:?}", e);
            return Err(e);
        }

        match command {
            SifliUartCommand::Exit => Ok(SifliUartResponse::Exit),
            _ => Self::recv(&mut self.reader),
        }
    }
}

#[allow(unused)]
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

    fn set_scan_chain(&mut self, scan_chain: Vec<ScanChainElement>) -> Result<(), DebugProbeError> {
        Ok(())
    }

    fn scan_chain(&self) -> Result<&[ScanChainElement], DebugProbeError> {
        Ok(&[])
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

    fn try_get_arm_interface<'probe>(
        self: Box<Self>,
    ) -> Result<Box<dyn UninitializedArmProbe + 'probe>, (Box<dyn DebugProbe>, DebugProbeError)>
    {
        Ok(Box::new(UninitializedSifliUartArmProbe { probe: self }))
    }

    fn has_riscv_interface(&self) -> bool {
        false
    }

    fn has_xtensa_interface(&self) -> bool {
        false
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
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
                    .contains("Sifli"))
        {
            return None;
        }

        let vendor_id = usb_info.vid;
        let product_id = usb_info.pid;
        let serial_number = Some(port_name.to_string()); //We set serial_number to the serial device number to make it easier to specify the
        let hid_interface = usb_info.interface;
        let identifier = "Sifli uart debug probe".to_string();

        Some(DebugProbeInfo {
            identifier,
            vendor_id,
            product_id,
            serial_number,
            probe_factory: &SifliUartFactory,
            hid_interface,
        })
    }

    fn open_port(&self, port_name: &str) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        let port = serialport::new(port_name, DEFUALT_UART_BAUD)
            .timeout(Duration::from_secs(3))
            .open()
            .map_err(|_| {
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
