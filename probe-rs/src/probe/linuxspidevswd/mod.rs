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

use crate::{
    CoreStatus,
    architecture::arm::{
        ArmCommunicationInterface, ArmDebugInterface, ArmError, DapError, DapProbe, RawDapAccess,
        RegisterAddress,
        dp::{DpRegister, RdBuff},
        sequences::ArmDebugSequence,
    },
    probe::{
        DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, ProbeCreationError,
        ProbeError, ProbeFactory, SwdSettings, WireProtocol,
    },
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
const SWD_LINE_RESET_BITS: u8 = 51;
const SWD_LINE_RESET_ONES: u64 = 0x0007_FFFF_FFFF_FFFF;

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

    fn list_probes(&self) -> Vec<DebugProbeInfo> {
        list_spidev_links()
            .into_iter()
            .map(|path| probe_info_for_path(&path))
            .collect()
    }

    fn list_probes_filtered(&self, selector: Option<&DebugProbeSelector>) -> Vec<DebugProbeInfo> {
        let Some(selector) = selector else {
            return self.list_probes();
        };

        if selector
            .serial_number
            .as_deref()
            .is_some_and(|serial| is_valid_spidev_path(Path::new(serial)))
        {
            return vec![probe_info_for_path(Path::new(
                selector.serial_number.as_deref().unwrap(),
            ))];
        }

        self.list_probes()
            .into_iter()
            .filter(|probe| selector.matches_probe(probe))
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
    fn transfer(&mut self, packet_size: usize) -> Result<impl Iterator<Item = u64>, ArmError> {
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
    fn flush_writes(&mut self) -> Result<(), ArmError> {
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

    fn raw_read_block_internal(
        &mut self,
        address: RegisterAddress,
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        // Flush any queued writes.
        self.flush_writes()?;

        self.tx_buffer
            .reserve((values.len() + 1) * READ_PACKET_SIZE);
        let packet = SwdReadPacket::new(address);
        for _ in 0..values.len() {
            let packet = packet.0.reverse_bits().to_be_bytes();
            self.tx_buffer
                .extend_from_slice(&packet[0..READ_PACKET_SIZE]);
        }

        if address.is_ap() {
            // AP reads are pipelined. We need to insert a read to Dap:RDBUFF to get
            // the actual return value for the final read.
            let packet = SwdReadPacket::new(RegisterAddress::DpRegister(RdBuff::ADDRESS));
            let packet = packet.0.reverse_bits().to_be_bytes();
            self.tx_buffer
                .extend_from_slice(&packet[0..READ_PACKET_SIZE]);
        }

        // Do the transfer and read results.
        let mut i = 0;
        let mut skip_packet = address.is_ap();
        for packet in self.transfer(READ_PACKET_SIZE)? {
            let response = SwdReadPacket(packet);
            parse_swd_ack(response.ack())?;

            // Check RDATA parity bit.
            let parity = (response.data().count_ones() & 1) == 1;
            if parity != response.parity2() {
                return Err(ArmError::Dap(DapError::IncorrectParity));
            }

            // Possibly skip the first packet.
            if skip_packet {
                skip_packet = false;
                continue;
            }

            values[i] = response.data();
            i += 1;
        }

        Ok(())
    }

    fn raw_write_block_internal(
        &mut self,
        address: RegisterAddress,
        values: &[u32],
    ) -> Result<(), ArmError> {
        self.tx_buffer.reserve(values.len() * WRITE_PACKET_SIZE);
        let mut packet = SwdWritePacket::new(address, 0);
        for &value in values {
            SwdWritePacket::update_data(&mut packet, value);

            let packet = packet.0.reverse_bits().to_be_bytes();
            self.tx_buffer
                .extend_from_slice(&packet[0..WRITE_PACKET_SIZE]);

            // If there isn't space for another write packet plus idle cycles, flush the queue.
            let available = MAX_QUEUE_BYTES - self.tx_buffer.len() - 1;
            if available < WRITE_PACKET_SIZE {
                self.flush_writes()?;
            }
        }

        Ok(())
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

    fn detach(&mut self) -> Result<(), crate::Error> {
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

    fn try_get_arm_debug_interface<'probe>(
        self: Box<Self>,
        sequence: std::sync::Arc<dyn ArmDebugSequence>,
    ) -> Result<Box<dyn ArmDebugInterface + 'probe>, (Box<dyn DebugProbe>, ArmError)> {
        Ok(ArmCommunicationInterface::create(
            self, sequence, /* use_overrun_detect*/ false,
        ))
    }
}

impl DapProbe for LinuxSpidevSwdProbe {}

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

fn parse_swd_ack(ack: u64) -> Result<(), DapError> {
    // These are the little-endian interpretations of bits,
    // so appear backwards relative to the wire order.
    match ack {
        0b001 => Ok(()),
        0b010 => Err(DapError::WaitResponse),
        0b100 => Err(DapError::FaultResponse),
        0b111 => Err(DapError::NoAcknowledge),
        _ => Err(DapError::Protocol(WireProtocol::Swd)),
    }
}

impl RawDapAccess for LinuxSpidevSwdProbe {
    fn raw_read_register(&mut self, address: RegisterAddress) -> Result<u32, ArmError> {
        let mut data = 0;
        match self.raw_read_block_internal(address, std::slice::from_mut(&mut data)) {
            Ok(()) => Ok(data),
            Err(ArmError::Dap(DapError::WaitResponse)) => {
                tracing::debug!("Read from {address:?} got WAIT response, retrying once");
                self.raw_read_block_internal(address, std::slice::from_mut(&mut data))?;
                Ok(data)
            }
            Err(e) => Err(e),
        }
    }

    fn raw_write_register(&mut self, address: RegisterAddress, value: u32) -> Result<(), ArmError> {
        match self.raw_write_block(address, &[value]) {
            Ok(()) => Ok(()),
            Err(ArmError::Dap(DapError::WaitResponse)) => {
                tracing::debug!("Write to {address:?} got WAIT response, retrying once");
                self.raw_write_block_internal(address, &[value])?;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn raw_read_block(
        &mut self,
        address: RegisterAddress,
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        // Flush any queued writes.
        match self.raw_read_block_internal(address, values) {
            Ok(()) => Ok(()),
            Err(ArmError::Dap(DapError::WaitResponse)) => {
                tracing::debug!("Read from {address:?} got WAIT response, retrying once");
                self.raw_read_block_internal(address, values)
            }
            Err(e) => Err(e),
        }
    }

    fn raw_write_block(
        &mut self,
        address: RegisterAddress,
        values: &[u32],
    ) -> Result<(), ArmError> {
        match self.raw_write_block_internal(address, values) {
            Ok(()) => Ok(()),
            Err(ArmError::Dap(DapError::WaitResponse)) => {
                tracing::debug!("Write to {address:?} got WAIT response, retrying once");
                self.raw_write_block_internal(address, values)
            }
            Err(e) => Err(e),
        }
    }

    fn raw_flush(&mut self) -> Result<(), ArmError> {
        self.flush_writes()
    }

    fn jtag_sequence(&mut self, _cycles: u8, _tms: bool, _tdi: u64) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "jtag_sequence",
        })
    }

    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError> {
        let tx = encode_swj_sequence(bit_len, bits);
        self.transfer_raw_bytes(&tx)
    }

    fn swj_pins(
        &mut self,
        _pin_out: u32,
        _pin_select: u32,
        _pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "swj_pins",
        })
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn core_status_notification(&mut self, _state: CoreStatus) -> Result<(), DebugProbeError> {
        Ok(())
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
    fn new(address: RegisterAddress, data: u32) -> Self {
        let mut packet = SwdWritePacket(0);
        packet.set_start(true);
        packet.set_ap_n_dp(address.is_ap());
        packet.set_r_n_w(false);
        packet.set_a2(address.a2());
        packet.set_a3(address.a3());
        packet.set_parity1(address.is_ap() ^ false ^ address.a2() ^ address.a3());
        packet.set_stop(false);
        packet.set_park(true);
        packet.set_data(data);
        packet.set_parity2((data.count_ones() & 1) == 1);
        packet
    }

    fn update_data(this: &mut Self, data: u32) {
        this.set_data(data);
        this.set_parity2((data.count_ones() & 1) == 1);
    }
}

impl SwdReadPacket {
    fn new(address: RegisterAddress) -> Self {
        let mut packet = SwdReadPacket(0);
        packet.set_start(true);
        packet.set_ap_n_dp(address.is_ap());
        packet.set_r_n_w(true);
        packet.set_a2(address.a2());
        packet.set_a3(address.a3());
        packet.set_parity1(address.is_ap() ^ true ^ address.a2() ^ address.a3());
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

fn encode_swj_sequence(bit_len: u8, bits: u64) -> Vec<u8> {
    assert!(bit_len <= 64);

    if bit_len == 0 {
        return Vec::new();
    }

    if is_line_reset_pattern(bit_len, bits) {
        let send_bits = bit_len.div_ceil(8) * 8;
        let remaining_low_cycles = bit_len.saturating_sub(SWD_LINE_RESET_BITS);
        let reset_sequence = 0xFFFF_FFFF_FFFF_FFFF_u64 >> (64 - send_bits + remaining_low_cycles);

        let mut tx: Vec<u8> =
            reset_sequence.to_le_bytes()[0..bit_len.div_ceil(8) as usize].to_vec();
        tx = tx.into_iter().map(|b: u8| b.reverse_bits()).collect();
        return tx;
    }

    let mut tx: Vec<u8> = bits.to_le_bytes()[0..bit_len.div_ceil(8) as usize].to_vec();
    tx = tx.into_iter().map(|b: u8| b.reverse_bits()).collect();
    tx
}

fn is_line_reset_pattern(bit_len: u8, bits: u64) -> bool {
    if bit_len < SWD_LINE_RESET_BITS {
        return false;
    }

    let lower_bits_are_ones = (bits & SWD_LINE_RESET_ONES) == SWD_LINE_RESET_ONES;
    let upper_bits_are_zero = (bits >> SWD_LINE_RESET_BITS) == 0;

    lower_bits_are_ones && upper_bits_are_zero
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(probes[0].serial_number.as_deref(), Some("/dev/spidev0.0"));
    }

    #[test]
    fn encode_swj_sequence_matches_switch_bytes() {
        assert_eq!(encode_swj_sequence(16, 0xE79E), vec![0x79, 0xE7]);
    }

    #[test]
    fn encode_swj_sequence_rounds_line_reset_up_with_high_padding() {
        assert_eq!(encode_swj_sequence(51, SWD_LINE_RESET_ONES), vec![0xFF; 7]);
    }

    #[test]
    fn encode_swj_sequence_rounds_line_reset_low_suffix_to_zero_bytes() {
        assert_eq!(
            encode_swj_sequence(53, SWD_LINE_RESET_ONES),
            vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFC]
        );
    }
}
