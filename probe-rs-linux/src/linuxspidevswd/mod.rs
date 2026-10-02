//! A probe that uses Linux spidev to emulate SWD using full-duplex SPI transfers.
//!
//! This implementation is designed to be broadly compatible with different SPI peripherals,
//! so doesn't use uncommon features such as 3-wire SPI or LSB-first transfers.
//!
//! The SPI output (PICO) and input (POCI) lines of the host must be tied together with a resistor.
//! This enables the host to drive the SWDIO line for host send phases and read the SWDIO line
//! during target send phases, while still allowing the target's SWD port to drive the line.
//!
//! For example:
//!
//! ```text
//!    Host                     Target
//! +--------+     1K         +--------+
//! |    PICO|---/\/\/\---+   |        |
//! |        |            |   |        |
//! |    POCI|------------+---|SWDIO   |
//! |        |                |        |
//! |     SCK|----------------|SWDCLK  |
//! +--------+                +--------+
//! ```
//!
//! The exact choice of resistor value depends on SWD clock speed and target/host
//! drive strength. However, 1 kilo-ohm is a good starting value, and has been found
//! to work well at speeds of 18 MHz.
//!
//! Any explicit probe selection may use a synthetic selector of the form
//! `0:0:/dev/spidevX.Y`, for example `0:0:/dev/spidev0.0`.
//! *For safety*, probe listing only exposes explicit `/dev/spidev_swd*` udev-links, so probe-rs
//! does not implicitly touch every SPI bus on the system.
//! If you want probe-rs list to search for devices on a SPI bus, you can create such a link with a udev rule:
//! ```bash
//! sudo tee /etc/udev/rules.d/99-spidev-swd.rules <<EOF
//! SUBSYSTEM=="spi", KERNEL=="spidev*", SYMLINK+="spidev_swd%n"
//! EOF
//! ```

use probe_rs::probe::{
    BatchError, BatchExecutionError, BitSequence, CommandResult, DebugProbe, DebugProbeError,
    DebugProbeInfo, DebugProbeSelector, ProbeCreationError, ProbeError, ProbeFactory, Results,
    SwdSettings, WireProtocol,
    list::ProbeListItem,
    swd::{Direction, Port, SwdBatch, SwdOp, SwdProbe, SwdTransferError},
};
use spidev::{SpiModeFlags, SpidevOptions, SpidevTransfer};
use std::fmt::Debug;
use std::path::{Path, PathBuf};

const LINUX_SPIDEV_SWD_IDENTIFIER: &str = "Linux spidev SWD";
const SPIDEV_DIR: &str = "/dev";
const SPIDEV_PREFIX: &str = "spidev";
const SPIDEV_LIST_PREFIX: &str = "spidev_swd";

const WRITE_PACKET_SIZE: usize = 8; // 7 byte writes work, but only for slower speeds. has little impact on overall performance.
const READ_PACKET_SIZE: usize = 7;
/// Maximum number of bytes allowed in a single SPI transaction.
const MAX_QUEUE_BYTES: usize = 4096;
const SWD_LINE_RESET_BITS: usize = 51;

/// A factory for creating [`LinuxSpidevSwdProbe`] instances.
#[derive(Debug)]
pub struct LinuxSpidevSwdFactory;

impl std::fmt::Display for LinuxSpidevSwdFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(LINUX_SPIDEV_SWD_IDENTIFIER)
    }
}

impl ProbeFactory for LinuxSpidevSwdFactory {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        let path = find_matching_device(selector)?;
        let spidev = spidev::Spidev::open(&path).map_err(|error| {
            DebugProbeError::ProbeCouldNotBeCreated(match error.kind() {
                std::io::ErrorKind::NotFound => ProbeCreationError::NotFound,
                _ => ProbeCreationError::CouldNotOpen,
            })
        })?;

        Ok(Box::new(LinuxSpidevSwdProbe::new(spidev)))
    }

    fn list_probes(&self) -> Vec<ProbeListItem> {
        list_spidev_links()
            .into_iter()
            .map(|path| ProbeListItem::accessible(probe_info_for_path(&path)))
            .collect()
    }

    fn list_probes_filtered(&self, selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem> {
        let Some(selector) = selector else {
            return self.list_probes();
        };

        if selector
            .serial_number
            .as_deref()
            .is_some_and(|serial| is_valid_spidev_path(Path::new(serial)))
        {
            return vec![ProbeListItem::accessible(probe_info_for_path(Path::new(
                selector.serial_number.as_deref().unwrap(),
            )))];
        }

        self.list_probes()
            .into_iter()
            .filter(|probe| selector.matches_probe(&probe.info))
            .collect()
    }
}

/// One write in a [`WriteQueue`].
#[derive(Debug, Clone, Copy)]
struct QueuedWrite {
    /// The byte offset of the packet in the queue.
    offset: usize,
    /// The index of the operation in the batch.
    index: usize,
}

/// Write packets and idle bytes that go out in one SPI transfer.
#[derive(Debug, Default)]
struct WriteQueue {
    tx: Vec<u8>,
    writes: Vec<QueuedWrite>,
}

impl WriteQueue {
    fn is_empty(&self) -> bool {
        self.tx.is_empty()
    }

    /// Returns the bytes that still fit, after the `trailing` idle bytes of the transfer.
    fn room(&self, trailing: usize) -> usize {
        MAX_QUEUE_BYTES.saturating_sub(self.tx.len() + trailing)
    }

    fn push_write(&mut self, index: usize, packet: &[u8]) {
        self.writes.push(QueuedWrite {
            offset: self.tx.len(),
            index,
        });
        self.tx.extend_from_slice(packet);
    }

    /// Queues up to `bytes` idle bytes that fit before `trailing`, and returns the number.
    fn push_idle(&mut self, bytes: usize, trailing: usize) -> usize {
        let bytes = bytes.min(self.room(trailing));
        self.tx.extend(std::iter::repeat_n(0u8, bytes));
        bytes
    }

    /// Checks the ACK of every write in `rx`, the bytes received for the queue.
    ///
    /// On a failure, returns the batch index of the first write that failed.
    fn check_acks(&self, rx: &[u8]) -> Result<(), (usize, DebugProbeError)> {
        for write in &self.writes {
            let packet = decode_packet(&rx[write.offset..write.offset + WRITE_PACKET_SIZE]);
            parse_swd_ack(SwdWritePacket(packet).ack()).map_err(|error| (write.index, error))?;
        }
        Ok(())
    }

    fn clear(&mut self) {
        self.tx.clear();
        self.writes.clear();
    }
}

/// Converts the received bytes of one packet to the bit order of the packet bitfields.
fn decode_packet(bytes: &[u8]) -> u64 {
    let mut data = [0u8; 8];
    data[0..bytes.len()].copy_from_slice(bytes);
    u64::from_be_bytes(data).reverse_bits()
}

/// A full-duplex SPI bus.
trait SpiBus: Debug + Send {
    /// Sets the clock speed in Hz.
    fn configure(&mut self, speed_hz: u32) -> std::io::Result<()>;

    /// Clocks out `tx` and fills `rx`, of the same length, with the bytes that come back.
    fn transfer(&mut self, tx: &[u8], rx: &mut [u8]) -> std::io::Result<()>;
}

impl SpiBus for spidev::Spidev {
    fn configure(&mut self, speed_hz: u32) -> std::io::Result<()> {
        let options = SpidevOptions::new()
            .bits_per_word(8)
            .max_speed_hz(speed_hz)
            .mode(SpiModeFlags::SPI_MODE_3)
            .build();
        spidev::Spidev::configure(self, &options)
    }

    fn transfer(&mut self, tx: &[u8], rx: &mut [u8]) -> std::io::Result<()> {
        // The kernel takes the length from `tx` and writes that many bytes into `rx`.
        debug_assert_eq!(tx.len(), rx.len());
        let mut transfer = SpidevTransfer::read_write(tx, rx);
        spidev::Spidev::transfer(self, &mut transfer)
    }
}

fn io_error(error: std::io::Error) -> DebugProbeError {
    DebugProbeError::ProbeSpecific(LinuxSpidevSwdError::Io(error).into())
}

/// Probe using Linux spidev to emulate SWD with full-duplex SPI.
pub struct LinuxSpidevSwdProbe {
    bus: Box<dyn SpiBus>,
    speed_khz: u32,
    swd_settings: SwdSettings,

    queue: WriteQueue,
    rx_buffer: Vec<u8>,
}

impl Debug for LinuxSpidevSwdProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinuxSpidevSwdProbe")
            .field("bus", &self.bus)
            .field("speed_khz", &self.speed_khz)
            .finish_non_exhaustive()
    }
}

impl LinuxSpidevSwdProbe {
    /// Construct a new spidev SWD probe for the given SPI port.
    pub fn new(spidev: spidev::Spidev) -> Self {
        Self::with_bus(Box::new(spidev))
    }

    fn with_bus(bus: Box<dyn SpiBus>) -> Self {
        LinuxSpidevSwdProbe {
            bus,
            speed_khz: 1000,
            swd_settings: SwdSettings::default(),
            queue: WriteQueue::default(),
            rx_buffer: vec![0; MAX_QUEUE_BYTES],
        }
    }

    /// Configure the spidev device.
    fn configure_spidev(&mut self) -> Result<(), DebugProbeError> {
        self.bus.configure(self.speed_khz * 1000).map_err(io_error)
    }

    /// Idle bytes that follow every transfer, as `SwdSettings` requires, capped so that one
    /// write packet still fits in the queue.
    fn trailing_idle_bytes(&self) -> usize {
        self.swd_settings
            .idle_cycles_after_transfer
            .div_ceil(8)
            .min(MAX_QUEUE_BYTES - WRITE_PACKET_SIZE)
    }

    /// Sends the queued writes and checks their ACKs.
    ///
    /// On a failure, returns the batch index of the first write that failed, or `index` if
    /// the transfer itself failed. The queue is empty afterwards in either case.
    fn flush_writes(&mut self, index: usize) -> Result<(), (usize, DebugProbeError)> {
        if self.queue.is_empty() {
            return Ok(());
        }

        let index = self.queue.writes.first().map_or(index, |write| write.index);
        self.queue.push_idle(self.trailing_idle_bytes(), 0);
        let len = self.queue.tx.len();
        let result = self
            .bus
            .transfer(&self.queue.tx, &mut self.rx_buffer[0..len])
            .map_err(|e| (index, io_error(e)))
            .and_then(|()| self.queue.check_acks(&self.rx_buffer[0..len]));
        self.queue.clear();
        result
    }

    fn transfer_raw_bytes(&mut self, tx: &[u8]) -> Result<(), DebugProbeError> {
        let mut rx = vec![0; tx.len()];
        self.bus.transfer(tx, &mut rx).map_err(io_error)
    }

    fn queue_write(
        &mut self,
        index: usize,
        is_ap: bool,
        addr: u8,
        data: u32,
    ) -> Result<(), (usize, DebugProbeError)> {
        if self.queue.room(self.trailing_idle_bytes()) < WRITE_PACKET_SIZE {
            self.flush_writes(index)?;
        }
        let packet = SwdWritePacket::new(is_ap, addr, data);
        let packet = packet.0.reverse_bits().to_be_bytes();
        self.queue.push_write(index, &packet[0..WRITE_PACKET_SIZE]);
        Ok(())
    }

    fn queue_idle(&mut self, index: usize, cycles: u32) -> Result<(), (usize, DebugProbeError)> {
        let mut remaining = cycles.div_ceil(8) as usize;
        while remaining > 0 {
            let queued = self.queue.push_idle(remaining, self.trailing_idle_bytes());
            if queued == 0 {
                self.flush_writes(index)?;
            }
            remaining -= queued;
        }
        Ok(())
    }

    /// Sends one read as its own transfer. The queue must be empty.
    fn transfer_read(&mut self, is_ap: bool, addr: u8) -> Result<u32, DebugProbeError> {
        debug_assert!(self.queue.is_empty());
        let packet = SwdReadPacket::new(is_ap, addr);
        let packet = packet.0.reverse_bits().to_be_bytes();
        let mut tx = packet[0..READ_PACKET_SIZE].to_vec();
        tx.extend(std::iter::repeat_n(0u8, self.trailing_idle_bytes()));
        self.bus
            .transfer(&tx, &mut self.rx_buffer[0..tx.len()])
            .map_err(io_error)?;

        let response = SwdReadPacket(decode_packet(&self.rx_buffer[0..READ_PACKET_SIZE]));
        parse_swd_ack(response.ack())?;

        let parity = (response.data().count_ones() & 1) == 1;
        if parity != response.parity2() {
            return Err(DebugProbeError::SwdTransfer(
                SwdTransferError::IncorrectParity,
            ));
        }

        Ok(response.data())
    }
}

impl DebugProbe for LinuxSpidevSwdProbe {
    fn get_name(&self) -> &str {
        "spidev SWD"
    }

    fn speed_khz(&self) -> u32 {
        self.speed_khz
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        let prev_speed_khz = self.speed_khz;
        self.speed_khz = speed_khz;
        if let Err(e) = self.configure_spidev() {
            self.speed_khz = prev_speed_khz;
            return Err(e);
        }
        Ok(self.speed_khz)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        self.configure_spidev()
    }

    fn detach(&mut self) -> Result<(), probe_rs::Error> {
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "target_reset",
        })
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "target_reset_assert",
        })
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "target_reset_deassert",
        })
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        if protocol != WireProtocol::Swd {
            Err(DebugProbeError::UnsupportedProtocol(protocol))
        } else {
            Ok(())
        }
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        Some(WireProtocol::Swd)
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn has_arm_interface(&self) -> bool {
        true
    }

    fn try_as_swd_probe(self: Box<Self>) -> Result<Box<dyn SwdProbe>, Box<dyn DebugProbe>> {
        Ok(self)
    }
}

impl SwdProbe for LinuxSpidevSwdProbe {
    fn run_batch(
        &mut self,
        batch: &SwdBatch,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        let mut results = Results::new();

        for (index, (id, op)) in batch.iter().enumerate() {
            let at_index = |error| (index, error);
            let op_result = match op {
                SwdOp::Transfer {
                    port,
                    addr,
                    direction: Direction::Write,
                    data,
                } => self.queue_write(index, *port == Port::Ap, *addr, *data),
                SwdOp::Transfer {
                    port,
                    addr,
                    direction: Direction::Read,
                    ..
                } => self.flush_writes(index).and_then(|()| {
                    let value = self
                        .transfer_read(*port == Port::Ap, *addr)
                        .map_err(at_index)?;
                    if id.should_capture() {
                        results.push(id, CommandResult::U32(value));
                    }
                    Ok(())
                }),
                SwdOp::Sequence(bits) => self.flush_writes(index).and_then(|()| {
                    self.transfer_raw_bytes(&encode_swj_sequence(bits))
                        .map_err(at_index)
                }),
                SwdOp::Idle { cycles } => self.queue_idle(index, *cycles),
                SwdOp::Pins { .. } => self.flush_writes(index).and_then(|()| {
                    Err(at_index(DebugProbeError::NotImplemented {
                        function_name: "swj_pins",
                    }))
                }),
            };

            if let Err((fault_operation, error)) = op_result {
                self.queue.clear();
                return Err(batch_error(error, results, fault_operation));
            }
        }

        if let Err((fault_operation, error)) = self.flush_writes(batch.len().saturating_sub(1)) {
            return Err(batch_error(error, results, fault_operation));
        }

        Ok(results)
    }

    fn swd_settings(&self) -> SwdSettings {
        self.swd_settings.clone()
    }
}

/// Reports an SWD transfer error as specific to the batch, and any other error as a probe error.
fn batch_error(
    error: DebugProbeError,
    results: Results,
    fault_operation: usize,
) -> BatchExecutionError<DebugProbeError> {
    match error {
        DebugProbeError::SwdTransfer(transfer_error) => BatchExecutionError {
            error: BatchError::Specific(DebugProbeError::SwdTransfer(transfer_error)),
            results,
            fault_operation,
        },
        error => BatchExecutionError::new_from_debug_probe_at(error, results, fault_operation),
    }
}

fn find_matching_device(selector: &DebugProbeSelector) -> Result<PathBuf, DebugProbeError> {
    let Some(serial_number) = selector.serial_number.as_deref() else {
        return Err(DebugProbeError::ProbeCouldNotBeCreated(
            ProbeCreationError::NotFound,
        ));
    };

    let path = PathBuf::from(serial_number);
    if !is_valid_spidev_path(&path) || !path.exists() {
        Err(DebugProbeError::ProbeCouldNotBeCreated(
            ProbeCreationError::NotFound,
        ))
    } else {
        Ok(path)
    }
}

fn list_spidev_links() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(SPIDEV_DIR) else {
        return vec![];
    };

    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_valid_spidev_link(path))
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn is_valid_spidev_path(path: &Path) -> bool {
    path.parent() == Some(Path::new(SPIDEV_DIR))
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(SPIDEV_PREFIX))
}

fn is_valid_spidev_link(path: &Path) -> bool {
    path.parent() == Some(Path::new(SPIDEV_DIR))
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(SPIDEV_LIST_PREFIX))
}

fn probe_info_for_path(path: &Path) -> DebugProbeInfo {
    DebugProbeInfo::new(
        LINUX_SPIDEV_SWD_IDENTIFIER,
        0,
        0,
        Some(path.display().to_string()),
        &LinuxSpidevSwdFactory,
        None,
        false,
    )
}

fn parse_swd_ack(ack: u64) -> Result<(), DebugProbeError> {
    // These are the little-endian interpretations of bits,
    // so appear backwards relative to the wire order.
    match ack {
        0b001 => Ok(()),
        0b010 => Err(DebugProbeError::SwdTransfer(SwdTransferError::WaitResponse)),
        0b100 => Err(DebugProbeError::SwdTransfer(
            SwdTransferError::FaultResponse,
        )),
        0b111 => Err(DebugProbeError::SwdTransfer(
            SwdTransferError::NoAcknowledge,
        )),
        _ => Err(DebugProbeError::SwdTransfer(SwdTransferError::Protocol)),
    }
}

bitfield::bitfield! {
    /// A SWD write packet
    #[derive(Copy, Clone)]
    struct SwdWritePacket(u64);
    impl Debug;

    // Common header
    start, set_start: 0;
    ap_n_dp, set_ap_n_dp: 1;
    r_n_w, set_r_n_w: 2;
    a2, set_a2: 3;
    a3, set_a3: 4;
    parity1, set_parity1: 5;
    stop, set_stop: 6;
    park, set_park: 7;
    _, set_turnaround1: 8;
    ack, set_ack: 11, 9;

    // Write specific
    _, set_turnaround2: 12;
    u32, data, set_data: 44, 13;
    parity2, set_parity2: 45;
}

bitfield::bitfield! {
    /// A SWD read packet
    #[derive(Copy, Clone)]
    struct SwdReadPacket(u64);
    impl Debug;

    // Common header
    start, set_start: 0;
    ap_n_dp, set_ap_n_dp: 1;
    r_n_w, set_r_n_w: 2;
    a2, set_a2: 3;
    a3, set_a3: 4;
    parity1, set_parity1: 5;
    stop, set_stop: 6;
    park, set_park: 7;
    _, set_turnaround1: 8;
    ack, set_ack: 11, 9;

    // Read specific
    u32, data, set_data: 43, 12;
    parity2, set_parity2: 44;
    _, set_turnaround2: 45;
}

impl SwdWritePacket {
    fn new(is_ap: bool, addr: u8, data: u32) -> Self {
        let mut packet = SwdWritePacket(0);
        packet.set_start(true);
        packet.set_ap_n_dp(is_ap);
        packet.set_r_n_w(false);
        packet.set_a2((addr & 0b0100) != 0);
        packet.set_a3((addr & 0b1000) != 0);
        packet.set_parity1(is_ap ^ false ^ packet.a2() ^ packet.a3());
        packet.set_stop(false);
        packet.set_park(true);
        packet.set_data(data);
        packet.set_parity2((data.count_ones() & 1) == 1);
        packet
    }
}

impl SwdReadPacket {
    fn new(is_ap: bool, addr: u8) -> Self {
        let mut packet = SwdReadPacket(0);
        packet.set_start(true);
        packet.set_ap_n_dp(is_ap);
        packet.set_r_n_w(true);
        packet.set_a2((addr & 0b0100) != 0);
        packet.set_a3((addr & 0b1000) != 0);
        packet.set_parity1(is_ap ^ true ^ packet.a2() ^ packet.a3());
        packet.set_stop(false);
        packet.set_park(true);
        packet
    }
}

#[derive(Debug, thiserror::Error)]
enum LinuxSpidevSwdError {
    Io(std::io::Error),
}

impl core::fmt::Display for LinuxSpidevSwdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error {e}"),
        }
    }
}

impl ProbeError for LinuxSpidevSwdError {}

fn encode_swj_sequence(bits: &BitSequence) -> Vec<u8> {
    let bit_len = bits.len();
    if bit_len == 0 {
        return Vec::new();
    }

    if is_line_reset_pattern(bits) {
        // The high phase extends to the byte boundary. Only the requested idle cycles
        // stay low.
        let send_bits = bit_len.div_ceil(8) * 8;
        let high_bits = send_bits - (bit_len - SWD_LINE_RESET_BITS);

        let mut tx = vec![0u8; send_bits / 8];
        for i in 0..high_bits {
            tx[i / 8] |= 1 << (i % 8);
        }
        return tx.into_iter().map(|byte| byte.reverse_bits()).collect();
    }

    let mut tx = vec![0u8; bit_len.div_ceil(8)];
    for i in 0..bit_len {
        if bits[i] {
            tx[i / 8] |= 1 << (i % 8);
        }
    }
    tx.into_iter().map(|byte| byte.reverse_bits()).collect()
}

fn is_line_reset_pattern(bits: &BitSequence) -> bool {
    let bit_len = bits.len();
    if bit_len < SWD_LINE_RESET_BITS {
        return false;
    }

    for i in 0..SWD_LINE_RESET_BITS {
        if !bits[i] {
            return false;
        }
    }
    for i in SWD_LINE_RESET_BITS..bit_len {
        if bits[i] {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    const SWD_LINE_RESET_ONES: u64 = 0x0007_FFFF_FFFF_FFFF;

    #[test]
    fn probe_info_uses_spidev_path_as_serial() {
        let info = probe_info_for_path(Path::new("/dev/spidev_swd0"));

        assert_eq!(info.identifier, LINUX_SPIDEV_SWD_IDENTIFIER);
        assert_eq!(info.serial_number.as_deref(), Some("/dev/spidev_swd0"));
        assert_eq!(info.interface, None);
    }

    #[test]
    fn selector_matches_direct_spidev_probe_info() {
        let info = probe_info_for_path(Path::new("/dev/spidev1.0"));
        let selector: DebugProbeSelector = "0:0:/dev/spidev1.0".parse().unwrap();

        assert!(selector.matches_probe(&info));
    }

    #[test]
    fn validates_spidev_paths() {
        assert!(is_valid_spidev_path(Path::new("/dev/spidev0.0")));
        assert!(is_valid_spidev_path(Path::new("/dev/spidev_swd0")));
        assert!(!is_valid_spidev_path(Path::new("/tmp/spidev0.0")));
        assert!(!is_valid_spidev_path(Path::new("/dev/ttyUSB0")));
    }

    #[test]
    fn validates_spidev_listing_links() {
        assert!(is_valid_spidev_link(Path::new("/dev/spidev_swd")));
        assert!(is_valid_spidev_link(Path::new("/dev/spidev_swd0")));
        assert!(!is_valid_spidev_link(Path::new("/dev/spidev0.0")));
        assert!(!is_valid_spidev_link(Path::new("/tmp/spidev_swd")));
    }

    #[test]
    fn filtered_list_allows_direct_spidev_selector() {
        let selector: DebugProbeSelector = "0:0:/dev/spidev0.0".parse().unwrap();
        let probes = LinuxSpidevSwdFactory.list_probes_filtered(Some(&selector));

        assert_eq!(probes.len(), 1);
        assert_eq!(
            probes[0].info.serial_number.as_deref(),
            Some("/dev/spidev0.0")
        );
    }

    /// The bytes that a target returns for a write with `ack`, in wire order.
    fn write_reply(ack: u64) -> Vec<u8> {
        let mut packet = SwdWritePacket(0);
        packet.set_ack(ack);
        packet.0.reverse_bits().to_be_bytes()[0..WRITE_PACKET_SIZE].to_vec()
    }

    /// A queue of two writes, at batch indices 0 and 2, with one idle byte between them.
    fn two_writes() -> WriteQueue {
        let mut queue = WriteQueue::default();
        queue.push_write(0, &[0; WRITE_PACKET_SIZE]);
        queue.push_idle(1, 0);
        queue.push_write(2, &[0; WRITE_PACKET_SIZE]);
        queue
    }

    #[test]
    fn acks_are_read_at_each_write_after_idle_bytes() {
        let queue = two_writes();
        let mut rx = write_reply(0b001);
        rx.push(0);
        rx.extend(write_reply(0b001));
        assert!(queue.check_acks(&rx).is_ok());
    }

    #[test]
    fn the_earliest_failure_is_reported() {
        let queue = two_writes();
        let mut rx = write_reply(0b010);
        rx.push(0);
        rx.extend(write_reply(0b100));
        assert!(matches!(
            queue.check_acks(&rx),
            Err((
                0,
                DebugProbeError::SwdTransfer(SwdTransferError::WaitResponse)
            ))
        ));
    }

    /// A bus that records every transfer. It answers with the next scripted reply, or with an
    /// OK for a write packet at every multiple of `stride` bytes.
    #[derive(Debug)]
    struct FakeBus {
        sent: Arc<Mutex<Vec<usize>>>,
        replies: VecDeque<Vec<u8>>,
        stride: usize,
    }

    impl FakeBus {
        fn new(replies: impl IntoIterator<Item = Vec<u8>>, stride: usize) -> Self {
            Self {
                sent: Arc::default(),
                replies: replies.into_iter().collect(),
                stride,
            }
        }
    }

    impl SpiBus for FakeBus {
        fn configure(&mut self, _speed_hz: u32) -> std::io::Result<()> {
            Ok(())
        }

        fn transfer(&mut self, tx: &[u8], rx: &mut [u8]) -> std::io::Result<()> {
            self.sent.lock().unwrap().push(tx.len());
            let reply = self.replies.pop_front().unwrap_or_else(|| {
                let mut write = write_reply(0b001);
                write.resize(self.stride, 0);
                write.repeat(tx.len().div_ceil(self.stride))
            });
            rx.copy_from_slice(&reply[0..rx.len()]);
            Ok(())
        }
    }

    #[test]
    fn a_wait_on_a_queued_write_stops_the_batch_at_that_write() {
        let mut reply = write_reply(0b001);
        reply.push(0);
        reply.extend(write_reply(0b010));
        reply.push(0);
        let bus = FakeBus::new([reply], WRITE_PACKET_SIZE);
        let sent = bus.sent.clone();
        let mut probe = LinuxSpidevSwdProbe::with_bus(Box::new(bus));

        let mut batch = SwdBatch::new();
        batch.write(Port::Dp, 0x8, 1);
        batch.idle(2);
        batch.write(Port::Dp, 0x8, 2);
        let _ = batch.read(Port::Dp, 0x0);
        let error = probe.run_batch(&batch).unwrap_err();

        assert_eq!(error.fault_operation, 2);
        assert!(matches!(
            error.error,
            BatchError::Specific(DebugProbeError::SwdTransfer(SwdTransferError::WaitResponse))
        ));
        // The writes, the idle byte and the trailing idle byte; the read is not sent.
        assert_eq!(*sent.lock().unwrap(), [18]);
    }

    #[test]
    fn a_full_queue_is_sent_before_the_next_write() {
        // Each write is followed by one idle byte, as `SwdPort` schedules them.
        let bus = FakeBus::new([], WRITE_PACKET_SIZE + 1);
        let sent = bus.sent.clone();
        let mut probe = LinuxSpidevSwdProbe::with_bus(Box::new(bus));

        let mut batch = SwdBatch::new();
        for value in 0..600 {
            batch.write(Port::Ap, 0xC, value);
            batch.idle(2);
        }
        probe.run_batch(&batch).unwrap();

        // 455 writes with their idle bytes and the trailing idle byte fill the queue exactly;
        // the other 145 follow.
        assert_eq!(*sent.lock().unwrap(), [455 * 9 + 1, 145 * 9 + 1]);
    }

    #[test]
    fn a_wait_names_the_write_that_got_it() {
        let queue = two_writes();
        let mut rx = write_reply(0b001);
        rx.push(0);
        rx.extend(write_reply(0b010));
        assert!(matches!(
            queue.check_acks(&rx),
            Err((
                2,
                DebugProbeError::SwdTransfer(SwdTransferError::WaitResponse)
            ))
        ));
    }

    #[test]
    fn idle_bytes_stop_at_the_queue_limit() {
        let mut queue = WriteQueue::default();
        assert_eq!(queue.push_idle(MAX_QUEUE_BYTES, 1), MAX_QUEUE_BYTES - 1);
        assert_eq!(queue.room(1), 0);
        assert_eq!(queue.push_idle(8, 1), 0);
    }

    #[test]
    fn encode_swj_sequence_matches_switch_bytes() {
        assert_eq!(
            encode_swj_sequence(&BitSequence::from_u64(16, 0xE79E)),
            vec![0x79, 0xE7]
        );
    }

    #[test]
    fn encode_swj_sequence_rounds_line_reset_up_with_high_padding() {
        assert_eq!(
            encode_swj_sequence(&BitSequence::from_u64(51, SWD_LINE_RESET_ONES)),
            vec![0xFF; 7]
        );
    }

    #[test]
    fn encode_swj_sequence_rounds_line_reset_low_suffix_to_zero_bytes() {
        assert_eq!(
            encode_swj_sequence(&BitSequence::from_u64(53, SWD_LINE_RESET_ONES)),
            vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFC]
        );
    }
}
