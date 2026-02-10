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
//! to work well at speeds of 25 MHz.

use crate::{
    CoreStatus,
    architecture::arm::{
        ArmCommunicationInterface, ArmDebugInterface, ArmError, DapError, DapProbe, RawDapAccess,
        RegisterAddress,
        dp::{DpRegister, RdBuff},
        sequences::ArmDebugSequence,
    },
    probe::{DebugProbe, DebugProbeError, ProbeError, WireProtocol},
};
use spidev::{SpiModeFlags, SpidevOptions, SpidevTransfer};
use std::fmt::Debug;

const WRITE_PACKET_SIZE: usize = 7;
const READ_PACKET_SIZE: usize = 8;
/// Maximum number of bytes allowed in a single SPI transaction.
const MAX_QUEUE_BYTES: usize = 4096;

/// Probe using Linux spidev to emulate SWD with full-duplex SPI.
pub struct LinuxSpidevSwdProbe {
    spidev: spidev::Spidev,
    speed_khz: u32,

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

        // Add 8 extra idle cycles, because we aren't going to be immediately
        // sending an additional transaction after the last one.
        self.tx_buffer.extend_from_slice(&[0]);

        // Do the transfer and verify.
        for packet in self.transfer(WRITE_PACKET_SIZE)? {
            let response = SwdWritePacket(packet);
            parse_swd_ack(response.ack())?;
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
        self.raw_read_block(address, std::slice::from_mut(&mut data))?;
        Ok(data)
    }

    fn raw_write_register(&mut self, address: RegisterAddress, value: u32) -> Result<(), ArmError> {
        self.raw_write_block(address, &[value])
    }

    fn raw_read_block(
        &mut self,
        address: RegisterAddress,
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        // Flush any queued writes.
        self.flush_writes()?;

        // Build the tx buffer.
        self.tx_buffer
            .reserve((values.len() + 1) * READ_PACKET_SIZE);
        let packet = SwdReadPacket::new(address);
        for _ in 0..values.len() {
            let packet = packet.0.reverse_bits().to_be_bytes();
            self.tx_buffer.extend_from_slice(&packet);
        }

        if address.is_ap() {
            // AP reads are pipelined. We need to insert a read to Dap:RDBUFF to get
            // the actual return value for the final read.
            let packet = SwdReadPacket::new(RegisterAddress::DpRegister(RdBuff::ADDRESS));
            let packet = packet.0.reverse_bits().to_be_bytes();
            self.tx_buffer.extend_from_slice(&packet);
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

    fn raw_write_block(
        &mut self,
        address: RegisterAddress,
        values: &[u32],
    ) -> Result<(), ArmError> {
        // Build the tx buffer.
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

    fn raw_flush(&mut self) -> Result<(), ArmError> {
        self.flush_writes()
    }

    fn jtag_sequence(&mut self, _cycles: u8, _tms: bool, _tdi: u64) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "jtag_sequence",
        })
    }

    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError> {
        // Output bit_len bits, least-significant bit first.
        // We can only output bytes, so mask out unused bits and round up.
        let bits = bits & ((1u64 << bit_len) - 1);
        let tx = bits.reverse_bits().to_be_bytes();
        let mut rx = [0u8; 8];
        let num_bytes = bit_len.div_ceil(8) as usize;
        let mut transfer = SpidevTransfer::read_write(&tx[0..num_bytes], &mut rx[0..num_bytes]);
        self.spidev
            .transfer(&mut transfer)
            .map_err(|e| DebugProbeError::ProbeSpecific(LinuxSpidevSwdError::Io(e).into()))
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
