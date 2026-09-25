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

/// Probe using Linux spidev to emulate SWD with full-duplex SPI.
pub struct LinuxSpidevSwdProbe {
    spidev: spidev::Spidev,
    speed_khz: u32,
    swd_settings: SwdSettings,

    tx_buffer: Vec<u8>,
    rx_buffer: Vec<u8>,
}

impl Debug for LinuxSpidevSwdProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinuxSpidevSwdProbe")
            .field("spidev", &self.spidev)
            .field("speed_khz", &self.speed_khz)
            .finish_non_exhaustive()
    }
}

impl LinuxSpidevSwdProbe {
    /// Construct a new spidev SWD probe for the given SPI port.
    pub fn new(spidev: spidev::Spidev) -> Self {
        LinuxSpidevSwdProbe {
            spidev,
            speed_khz: 1000,
            swd_settings: SwdSettings::default(),
            tx_buffer: Vec::new(),
            rx_buffer: vec![0; MAX_QUEUE_BYTES],
        }
    }

    /// Configure the spidev device.
    fn configure_spidev(&mut self) -> Result<(), DebugProbeError> {
        let options = SpidevOptions::new()
            .bits_per_word(8)
            .max_speed_hz(self.speed_khz * 1000)
            .mode(SpiModeFlags::SPI_MODE_3)
            .build();
        self.spidev
            .configure(&options)
            .map_err(|e| DebugProbeError::ProbeSpecific(LinuxSpidevSwdError::Io(e).into()))
    }

    /// Transfer the TX buffer, packetize, and fix bit order.
    fn transfer(
        &mut self,
        packet_size: usize,
    ) -> Result<impl Iterator<Item = u64>, DebugProbeError> {
        // Add idle cycles after the transfer as required by SwdSettings.
        let idle_bytes = self.swd_settings.idle_cycles_after_transfer.div_ceil(8);
        self.tx_buffer.extend(std::iter::repeat_n(0u8, idle_bytes));

        assert!(packet_size <= 8);

        let rx_buffer = &mut self.rx_buffer[0..self.tx_buffer.len()];
        let mut transfer = SpidevTransfer::read_write(&self.tx_buffer, rx_buffer);

        let result = self.spidev.transfer(&mut transfer);
        self.tx_buffer.clear();
        result.map_err(|e| DebugProbeError::ProbeSpecific(LinuxSpidevSwdError::Io(e).into()))?;

        Ok(rx_buffer.chunks_exact(packet_size).map(move |packet| {
            let mut data = [0u8; 8];
            data[0..packet_size].copy_from_slice(packet);
            u64::from_be_bytes(data).reverse_bits()
        }))
    }

    /// Flush pending writes in the TX buffer.
    fn flush_writes(&mut self) -> Result<(), DebugProbeError> {
        if self.tx_buffer.is_empty() {
            return Ok(());
        }

        // Do the transfer and verify.
        for packet in self.transfer(WRITE_PACKET_SIZE)? {
            let response = SwdWritePacket(packet);
            parse_swd_ack(response.ack())?;
        }

        Ok(())
    }

    fn transfer_raw_bytes(&mut self, tx: &[u8]) -> Result<(), DebugProbeError> {
        let mut rx = vec![0; tx.len()];
        let mut transfer = SpidevTransfer::read_write(tx, &mut rx);
        self.spidev
            .transfer(&mut transfer)
            .map_err(|e| DebugProbeError::ProbeSpecific(LinuxSpidevSwdError::Io(e).into()))?;
        Ok(())
    }

    fn queue_write(&mut self, is_ap: bool, addr: u8, data: u32) -> Result<(), DebugProbeError> {
        self.tx_buffer.reserve(WRITE_PACKET_SIZE);
        let packet = SwdWritePacket::new(is_ap, addr, data);
        let packet = packet.0.reverse_bits().to_be_bytes();
        self.tx_buffer
            .extend_from_slice(&packet[0..WRITE_PACKET_SIZE]);

        let available = MAX_QUEUE_BYTES - self.tx_buffer.len() - 1;
        if available < WRITE_PACKET_SIZE {
            self.flush_writes()?;
        }

        Ok(())
    }

    fn queue_idle(&mut self, cycles: u32) -> Result<(), DebugProbeError> {
        let bytes = cycles.div_ceil(8) as usize;
        self.tx_buffer.extend(std::iter::repeat_n(0u8, bytes));
        Ok(())
    }

    fn transfer_read(&mut self, is_ap: bool, addr: u8) -> Result<u32, DebugProbeError> {
        self.flush_writes()?;

        let packet = SwdReadPacket::new(is_ap, addr);
        let packet = packet.0.reverse_bits().to_be_bytes();
        self.tx_buffer
            .extend_from_slice(&packet[0..READ_PACKET_SIZE]);

        let response = self
            .transfer(READ_PACKET_SIZE)?
            .next()
            .ok_or_else(|| DebugProbeError::Other("missing SWD read response".into()))?;
        let response = SwdReadPacket(response);
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

        for (fault_operation, (id, op)) in batch.iter().enumerate() {
            let op_result: Result<(), DebugProbeError> = match op {
                SwdOp::Transfer {
                    port,
                    addr,
                    direction,
                    data,
                } => match direction {
                    Direction::Write => self.queue_write(*port == Port::Ap, *addr, *data),
                    Direction::Read => self.transfer_read(*port == Port::Ap, *addr).map(|value| {
                        if id.should_capture() {
                            results.push(id, CommandResult::U32(value));
                        }
                    }),
                },
                SwdOp::Sequence(bits) => {
                    let tx = encode_swj_sequence(bits);
                    self.transfer_raw_bytes(&tx)
                }
                SwdOp::Idle { cycles } => self.queue_idle(*cycles),
                SwdOp::Pins { .. } => Err(DebugProbeError::NotImplemented {
                    function_name: "swj_pins",
                }),
            };

            if let Err(error) = op_result {
                return match error {
                    DebugProbeError::SwdTransfer(transfer_error) => Err(BatchExecutionError {
                        error: BatchError::Specific(DebugProbeError::SwdTransfer(transfer_error)),
                        results,
                        fault_operation,
                    }),
                    error => Err(BatchExecutionError::new_from_debug_probe_at(
                        error,
                        results,
                        fault_operation,
                    )),
                };
            }
        }

        if let Err(error) = self.flush_writes() {
            return Err(BatchExecutionError::new_from_debug_probe_at(
                error,
                results,
                batch.len().saturating_sub(1),
            ));
        }

        Ok(results)
    }

    fn swd_settings(&self) -> SwdSettings {
        self.swd_settings.clone()
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
