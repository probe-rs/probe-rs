//! Serial ports to the target, for example the CDC-ACM port of a debug probe.

use std::{
    collections::VecDeque,
    fmt,
    io::{self, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use serialport::SerialPort;

use super::usb_util::{UsbSerialFilter, UsbSerialPort, usb_serial_ports};

/// The state of a modem control line (DTR or RTS).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineState {
    /// Do not change the line.
    #[default]
    Keep,
    /// Deassert the line.
    Low,
    /// Assert the line.
    High,
}

/// Settings of a target serial port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerialSettings {
    /// The baud rate. USB CDC devices often ignore it.
    pub baud: u32,
    /// The state of DTR after open.
    pub dtr: LineState,
    /// The state of RTS after open.
    pub rts: LineState,
}

impl Default for SerialSettings {
    fn default() -> Self {
        Self {
            baud: 115_200,
            dtr: LineState::Keep,
            rts: LineState::Keep,
        }
    }
}

/// An error of a target serial port.
#[derive(Debug, thiserror::Error)]
pub enum TargetSerialError {
    /// No serial port matches.
    #[error("No serial port matches {0}")]
    NotFound(String),
    /// More than one serial port matches.
    #[error("More than one serial port matches {0}")]
    Ambiguous(String),
    /// The port is not open.
    #[error("The serial port is not open")]
    NotOpen,
    /// An I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A serial port to the target.
///
/// A debug probe gives the port with [`DebugProbe::target_serial`](super::DebugProbe::target_serial)
/// before attach. After that, the port does not depend on the probe or the session. The port is
/// closed until [`open`](Self::open). To disable the port, do not open it. After `open`, more
/// than one thread can read and write the port at the same time.
pub trait TargetSerial: Send + Sync + fmt::Debug {
    /// The settings that the driver recommends. The caller can change them before [`open`](Self::open).
    fn default_settings(&self) -> SerialSettings;

    /// Opens the port and applies the line states.
    fn open(&mut self, settings: &SerialSettings) -> Result<(), TargetSerialError>;

    /// Reads received data. Waits up to `timeout` for data. Returns 0 if no data arrives.
    fn read(&self, buf: &mut [u8], timeout: Duration) -> Result<usize, TargetSerialError>;

    /// Drops the data that the port received and nobody read yet.
    ///
    /// Call this before the target boots, to drop the output of the previous firmware.
    fn discard_input(&self) -> Result<(), TargetSerialError>;

    /// Writes data to the target.
    fn write(&self, data: &[u8]) -> Result<usize, TargetSerialError>;
}

/// How long one read waits before the reader checks the stop flag.
const READ_TIMEOUT: Duration = Duration::from_millis(20);
/// How long a write waits for the target to take the data.
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);
/// The time between two attempts to open a lost port.
const REOPEN_INTERVAL: Duration = Duration::from_millis(500);
/// The receive buffer keeps at most this many bytes. Older bytes are dropped.
const RX_BUFFER_LIMIT: usize = 1024 * 1024;

/// A target serial port that the host OS gives, for example a USB CDC-ACM port or a USB-UART
/// bridge.
pub struct HostSerialPort {
    filter: UsbSerialFilter,
    default_settings: SerialSettings,
    open: Option<OpenPort>,
}

struct OpenPort {
    shared: Arc<Shared>,
    reader: Option<JoinHandle<()>>,
}

/// The state that the reader thread and the handle share.
struct Shared {
    /// A clone of the reader's port, for writes. `None` while the port is lost.
    writer: Mutex<Option<Box<dyn SerialPort>>>,
    rx: Mutex<VecDeque<u8>>,
    /// Signals new data in `rx`.
    received: Condvar,
    /// Counts the discards. The reader drops the data that it read before a discard.
    epoch: AtomicU64,
    stop: AtomicBool,
}

impl HostSerialPort {
    /// Creates a handle to the port that `filter` selects. The port is not open yet.
    pub fn new(filter: UsbSerialFilter, default_settings: SerialSettings) -> Self {
        Self {
            filter,
            default_settings,
            open: None,
        }
    }

    fn close(&mut self) {
        if let Some(mut open) = self.open.take() {
            open.shared.stop.store(true, Ordering::Relaxed);
            if let Some(reader) = open.reader.take() {
                _ = reader.join();
            }
        }
    }
}

impl fmt::Debug for HostSerialPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostSerialPort")
            .field("filter", &self.filter)
            .field("open", &self.open.is_some())
            .finish()
    }
}

impl TargetSerial for HostSerialPort {
    fn default_settings(&self) -> SerialSettings {
        self.default_settings.clone()
    }

    fn open(&mut self, settings: &SerialSettings) -> Result<(), TargetSerialError> {
        self.close();

        let (reader_port, writer_port) = open_port(&self.filter, settings)?;
        let shared = Arc::new(Shared {
            writer: Mutex::new(Some(writer_port)),
            rx: Mutex::new(VecDeque::new()),
            received: Condvar::new(),
            epoch: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        });

        let reader = std::thread::Builder::new()
            .name("target-serial".to_string())
            .spawn({
                let shared = shared.clone();
                let filter = self.filter.clone();
                let settings = settings.clone();
                move || read_loop(reader_port, &shared, &filter, &settings)
            })?;

        self.open = Some(OpenPort {
            shared,
            reader: Some(reader),
        });
        Ok(())
    }

    fn read(&self, buf: &mut [u8], timeout: Duration) -> Result<usize, TargetSerialError> {
        let open = self.open.as_ref().ok_or(TargetSerialError::NotOpen)?;
        let mut rx = open.shared.rx.lock();
        open.shared
            .received
            .wait_while_for(&mut rx, |rx| rx.is_empty(), timeout);
        let len = buf.len().min(rx.len());
        for (dst, src) in buf.iter_mut().zip(rx.drain(..len)) {
            *dst = src;
        }
        Ok(len)
    }

    fn discard_input(&self) -> Result<(), TargetSerialError> {
        let open = self.open.as_ref().ok_or(TargetSerialError::NotOpen)?;
        let mut rx = open.shared.rx.lock();
        open.shared.epoch.fetch_add(1, Ordering::Relaxed);
        // The OS buffer can hold data that the reader did not read yet.
        if let Some(writer) = open.shared.writer.lock().as_ref() {
            writer
                .clear(serialport::ClearBuffer::Input)
                .map_err(io::Error::from)?;
        }
        rx.clear();
        Ok(())
    }

    /// Writes until all data is written or 1 s passes. Returns the number of bytes written.
    fn write(&self, data: &[u8]) -> Result<usize, TargetSerialError> {
        let open = self.open.as_ref().ok_or(TargetSerialError::NotOpen)?;
        let mut writer = open.shared.writer.lock();
        let writer = writer
            .as_mut()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotConnected))?;

        let deadline = Instant::now() + WRITE_TIMEOUT;
        let mut written = 0;
        while written < data.len() && Instant::now() < deadline {
            match writer.write(&data[written..]) {
                Ok(len) => written += len,
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                Err(error) if written == 0 => return Err(error.into()),
                Err(_) => break,
            }
        }
        Ok(written)
    }
}

impl Drop for HostSerialPort {
    fn drop(&mut self) {
        self.close();
    }
}

/// Selects the one port that matches `filter`.
fn select_port(
    ports: Vec<UsbSerialPort>,
    filter: &UsbSerialFilter,
) -> Result<UsbSerialPort, TargetSerialError> {
    let mut matching = ports.into_iter().filter(|port| filter.matches(port));
    let Some(port) = matching.next() else {
        return Err(TargetSerialError::NotFound(filter.to_string()));
    };
    if matching.next().is_some() {
        return Err(TargetSerialError::Ambiguous(filter.to_string()));
    }
    Ok(port)
}

/// A handle to read and a handle to write the same port.
type PortPair = (Box<dyn SerialPort>, Box<dyn SerialPort>);

/// Opens the port and returns a handle to read and a handle to write.
fn open_port(
    filter: &UsbSerialFilter,
    settings: &SerialSettings,
) -> Result<PortPair, TargetSerialError> {
    let ports = usb_serial_ports().map_err(io::Error::from)?;
    let port = select_port(ports, filter)?;
    tracing::debug!("Opening target serial port {}", port.port_name);

    let mut serial = serialport::new(&port.port_name, settings.baud)
        .timeout(READ_TIMEOUT)
        .open()
        .map_err(io::Error::from)?;

    // Linux and macOS set DTR and RTS when the port opens. DTR low with RTS high resets an ESP
    // USB-Serial-JTAG, so set RTS before DTR. Windows changes DTR only together with an RTS
    // write, so write RTS again after DTR.
    let rts = (settings.rts != LineState::Keep).then_some(settings.rts == LineState::High);
    let dtr = (settings.dtr != LineState::Keep).then_some(settings.dtr == LineState::High);
    if let Some(rts) = rts {
        serial.write_request_to_send(rts).map_err(io::Error::from)?;
    }
    if let Some(dtr) = dtr {
        serial
            .write_data_terminal_ready(dtr)
            .map_err(io::Error::from)?;
        if let Some(rts) = rts {
            serial.write_request_to_send(rts).map_err(io::Error::from)?;
        }
    }

    let writer = serial.try_clone().map_err(io::Error::from)?;
    Ok((serial, writer))
}

fn read_loop(
    mut port: Box<dyn SerialPort>,
    shared: &Shared,
    filter: &UsbSerialFilter,
    settings: &SerialSettings,
) {
    let mut buf = [0; 4096];

    while !shared.stop.load(Ordering::Relaxed) {
        let epoch = shared.epoch.load(Ordering::Relaxed);
        match port.read(&mut buf) {
            Ok(len) => {
                let mut rx = shared.rx.lock();
                if shared.epoch.load(Ordering::Relaxed) != epoch {
                    // The data arrived before a discard.
                    continue;
                }
                rx.extend(&buf[..len]);
                let excess = rx.len().saturating_sub(RX_BUFFER_LIMIT);
                rx.drain(..excess);
                shared.received.notify_all();
            }
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) => {
                tracing::warn!("Lost target serial port {filter}: {error}");
                *shared.writer.lock() = None;
                let Some((reader, writer)) = reopen(shared, filter, settings) else {
                    return;
                };
                tracing::info!("Target serial port {filter} is back");
                port = reader;
                *shared.writer.lock() = Some(writer);
            }
        }
    }
}

/// Opens the port again. Returns `None` if the handle stops first.
fn reopen(
    shared: &Shared,
    filter: &UsbSerialFilter,
    settings: &SerialSettings,
) -> Option<PortPair> {
    loop {
        let deadline = Instant::now() + REOPEN_INTERVAL;
        while Instant::now() < deadline {
            if shared.stop.load(Ordering::Relaxed) {
                return None;
            }
            std::thread::sleep(READ_TIMEOUT);
        }
        if let Ok(ports) = open_port(filter, settings) {
            return Some(ports);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serialport::UsbPortInfo;

    fn port(name: &str, serial_number: &str) -> UsbSerialPort {
        UsbSerialPort {
            port_name: name.to_string(),
            info: UsbPortInfo {
                vid: 0x303a,
                pid: 0x1001,
                serial_number: Some(serial_number.to_string()),
                manufacturer: None,
                product: None,
                interface: Some(0),
            },
        }
    }

    fn filter(serial_number: Option<&str>) -> UsbSerialFilter {
        UsbSerialFilter {
            vendor_id: Some(0x303a),
            product_id: Some(0x1001),
            serial_number: serial_number.map(str::to_string),
            interface: Some(0),
        }
    }

    #[test]
    fn select_requires_exactly_one_port() {
        let ports = vec![port("COM3", "A"), port("COM4", "B")];

        let selected = select_port(ports.clone(), &filter(Some("B"))).unwrap();
        assert_eq!(selected.port_name, "COM4");

        assert!(matches!(
            select_port(ports.clone(), &filter(Some("C"))),
            Err(TargetSerialError::NotFound(_))
        ));
        assert!(matches!(
            select_port(ports, &filter(None)),
            Err(TargetSerialError::Ambiguous(_))
        ));
    }
}
