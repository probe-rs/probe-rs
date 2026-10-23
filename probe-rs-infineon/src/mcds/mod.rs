//! MCDS trace recording on AURIX emulation devices.
//!
//! An AURIX emulation device (ED) carries the Multi-Core Debug Solution (MCDS), an on-chip trace
//! block that records into the emulation memory (EMEM). This module drives a recording the way
//! the Infineon MCDS recorder library does and saves it as an `.mcds` file, which the Infineon
//! tools (MCDS trace viewer, decoder) open. Decoding the trace is left to those tools.
//!
//! The MCDS configuration (the register writes that set up the trace) comes from the Infineon
//! MCDS configurator. [`McdsConfig::parse`] reads it from an `.mcds` file (the
//! `START_OF_MCDS_CONFIG` section) or from a plain list of `ADDRESS VALUE` lines.
//!
//! A recording:
//!
//! 1. checks that the device has an EMEM (it is an ED) and that the MCDS is idle (OCS = 0),
//! 2. writes the configuration: device registers in address map 0, MCDS registers (`0xF9xx_xxxx`)
//!    and EMEM (`0x9Fxx_xxxx`) in the EEC address map 1, at their EEC addresses,
//! 3. waits for the recording time while it reads the FIFO fill level,
//! 4. stops the trace, reads it back through the trace port in 1 KiB blocks and writes the
//!    cleanup configuration.
//!
//! The register offsets and the sequence follow the TC2xx ED recorder (`mcdsrl` prototype,
//! calibrated against the Infineon MCDS Recorder Library 3.3.0).

use std::{
    fmt::Write as _,
    io,
    time::{Duration, Instant},
};

use probe_rs::architecture::tricore::bus::{
    AddressMap, BusTransfer, ResetKind, TriCoreBusAccess, TriCoreError,
};

/// The EMEM, readable in map 0 on an emulation device only.
const EMEM_PROBE: u32 = 0x5000_0000;
/// The MCDS control registers of the TC2xx ED (EEC map 1).
const TC2XX_ED_CTRL: u32 = 0xF00E_6000;
/// The MCDS FIFO and status registers of the TC2xx ED (EEC map 1).
const TC2XX_ED_STATUS: u32 = 0xF00E_8000;

/// MCDS control register offsets.
const OFF_OCS: u32 = 0x00_0004;
const OFF_ID: u32 = 0x00_0020;
const OFF_TRACE_CTRL: u32 = 0x00_0210;
const OFF_TRACE_PORT: u32 = 0x10_0000;
const OFF_TRACE_SIZE: u32 = 0x10_1030;
/// MCDS status register offsets.
const OFF_FIFO_FILL: u32 = 0x0200;
const OFF_FIFO_BASE: u32 = 0x0204;
const OFF_FIFO_TOP: u32 = 0x0208;
/// The value written to the trace control register to stop the trace.
const TRACE_STOP: u32 = 0x8400;

/// The register of the access hardware clock in the TAS server map, used as the MCDS clock.
const FREQUENCY_REGISTER: u32 = 0x000C_0100;
/// Lower values are not an MCDS clock.
const FREQUENCY_MIN: u32 = 2_000_000;

/// The configuration writes that start CPU0, one of which ends a configuration for a device
/// that was reset and halted (HAR).
const CPU0_START: [u32; 2] = [0xF000_0490, 0xFA10_1010];

/// The free-running 64-bit MCDS timestamp counter, in the TAS server map.
const TIMESTAMP: u32 = 0x5000_0090;

/// The size of one trace port read.
const BLOCK: usize = 1024;
/// How often the FIFO fill level is read during a recording.
const POLL: Duration = Duration::from_millis(100);

/// Errors of an MCDS recording.
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum McdsError {
    /// The MCDS configuration line {line} is not `ADDRESS VALUE` in hexadecimal: '{text}'
    ConfigSyntax {
        /// The line number.
        line: usize,
        /// The line.
        text: String,
    },
    /// The MCDS configuration has no register writes.
    EmptyConfig,
    /// The MCDS configuration does not set up the EMEM (no FIFO range and no MCDS ID write).
    NoEmemConfig,
    /// The configuration was made for JTAG ID {config:#010x}, the device is {device:#010x}.
    WrongDevice {
        /// The JTAG ID in the configuration.
        config: u32,
        /// The JTAG ID of the device.
        device: u32,
    },
    /// MCDS recording is not supported for the device with JTAG ID {0:#010x}.
    UnsupportedDevice(u32),
    /// The device has no EMEM: it is not an emulation device, so it has no MCDS.
    NotEmulationDevice,
    /// The MCDS registers (EEC address map 1) are not accessible through this probe: {0}
    McdsNotAccessible(TriCoreError),
    /// The MCDS is not idle (OCS = {0:#010x}): an earlier recording was not stopped, or another tool configured the MCDS. A device reset clears it.
    McdsBusy(u32),
    /// A reset recording needs a configuration that ends with the write that starts CPU0, not a write to {0:#010x}.
    HarWithoutCpuStart(u32),
    /// The device reported no trace data.
    NoTraceData,
    /// Device access failed: {0}
    Access(#[from] TriCoreError),
}

/// An MCDS configuration: register writes in the order they are applied.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McdsConfig {
    /// The MCDS device name of the configuration (`McdsDevice=` of an `.mcds` file).
    pub mcds_device: Option<String>,
    /// The JTAG ID the configuration was made for (`McdsDeviceUsed=` of an `.mcds` file).
    pub device_type: Option<u32>,
    /// The (address, value) writes, with device-view addresses.
    pub writes: Vec<(u32, u32)>,
}

impl McdsConfig {
    /// Parses a configuration from the text of an `.mcds` file, or from `ADDRESS VALUE` lines.
    ///
    /// In an `.mcds` file only the `START_OF_MCDS_CONFIG` section is used (the trace data after
    /// `START_OF_TDR` is not read). In a plain list, empty lines and lines starting with `#` are
    /// skipped.
    pub fn parse(text: &[u8]) -> Result<Self, McdsError> {
        // The trace data is binary; only the text before it matters.
        let text = match find(text, b"START_OF_TDR") {
            Some(end) => &text[..end],
            None => text,
        };
        let text = String::from_utf8_lossy(text);
        let mut config = Self::default();
        let in_mcds_file = text.contains("START_OF_MCDS_CONFIG");
        let mut in_section = !in_mcds_file;
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line == "START_OF_MCDS_CONFIG" {
                in_section = true;
                continue;
            }
            if !in_section {
                if let Some(name) = line.strip_prefix("McdsDevice=") {
                    config.mcds_device = Some(name.trim().to_string());
                } else if let Some(used) = line.strip_prefix("McdsDeviceUsed=") {
                    let id = used.split_whitespace().next().unwrap_or("");
                    config.device_type = parse_hex(id);
                }
                continue;
            }
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_whitespace();
            let pair = match (fields.next(), fields.next(), fields.next()) {
                (Some(address), Some(value), None) => parse_hex(address).zip(parse_hex(value)),
                _ => None,
            };
            let Some(pair) = pair else {
                return Err(McdsError::ConfigSyntax {
                    line: index + 1,
                    text: line.to_string(),
                });
            };
            config.writes.push(pair);
        }
        if config.writes.is_empty() {
            return Err(McdsError::EmptyConfig);
        }
        Ok(config)
    }
}

fn parse_hex(text: &str) -> Option<u32> {
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    u32::from_str_radix(&digits.replace('_', ""), 16).ok()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Where a configuration write goes: the device-view address of MCDS registers and EMEM is
/// translated to the EEC address map.
pub fn wire_address(address: u32) -> (AddressMap, u32) {
    match address >> 24 {
        // MCDS registers: 0xF90E_6xxx -> 0xF00E_6xxx.
        0xF9 => (AddressMap::Eec, address - 0x0900_0000),
        // EMEM: 0x9Fxx_xxxx -> 0xAFxx_xxxx.
        0x9F => (AddressMap::Eec, address + 0x1000_0000),
        _ => (AddressMap::Target, address),
    }
}

/// How a recording is made.
#[derive(Debug, Clone)]
pub struct RecordOptions {
    /// Reset and halt the device first (HAR). The configuration then ends with the write that
    /// starts CPU0, which is applied last.
    pub reset: bool,
    /// How long to record.
    pub duration: Duration,
    /// The configuration that returns the MCDS to idle after the recording.
    pub cleanup: Option<McdsConfig>,
}

/// The progress of a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// The trace data recorded so far, in KiB.
    pub trace_kb: u32,
    /// The time since the start.
    pub elapsed: Duration,
}

/// The local date and time of a recording, for the `.mcds` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timestamp {
    /// The year.
    pub year: i32,
    /// The month, 1 to 12.
    pub month: u8,
    /// The day, 1 to 31.
    pub day: u8,
    /// The hour, 0 to 23.
    pub hour: u8,
    /// The minute.
    pub minute: u8,
    /// The second.
    pub second: u8,
}

/// A finished recording.
#[derive(Debug, Clone)]
pub struct Recording {
    /// The MCDS device name, for example `TC2XX_ED`.
    pub mcds_device: String,
    /// The JTAG ID of the device.
    pub device_type: u32,
    /// The device name, for example `TC27xED`.
    pub device_name: String,
    /// The MCDS clock, if the probe knows it.
    pub frequency_hz: Option<u32>,
    /// When the recording started.
    pub started_at: Timestamp,
    /// The configuration that was written.
    pub config: Vec<(u32, u32)>,
    /// The trace data read from the trace port.
    pub trace: Vec<u8>,
    /// The recording time.
    pub elapsed: Duration,
}

impl Recording {
    /// Writes the recording in the `.mcds` format of the Infineon MCDS recorder: a text header,
    /// `START_OF_MCDS_CONFIG` with the (address, value) writes, then `START_OF_TDR` and the raw
    /// trace data.
    pub fn write_mcds(&self, out: &mut impl io::Write, tool: &str) -> io::Result<()> {
        let mut header = String::new();
        let t = &self.started_at;
        // Writing to a String cannot fail.
        let _ = writeln!(header, "McdsDevice={}", self.mcds_device);
        let _ = writeln!(
            header,
            "McdsDeviceUsed=0x{:08X}  ({})",
            self.device_type, self.device_name
        );
        let _ = writeln!(header, "McdsToolUsed=\"{tool}\"");
        if let Some(hz) = self.frequency_hz {
            let _ = writeln!(header, "McdsFrequencyMHz={:.1}", f64::from(hz) / 1e6);
        }
        let _ = writeln!(
            header,
            "RecordingDate={:04}-{:02}-{:02}",
            t.year, t.month, t.day
        );
        let _ = writeln!(
            header,
            "RecordingTime={:02}:{:02}:{:02}",
            t.hour, t.minute, t.second
        );
        header.push('\n');
        header.push_str("START_OF_MCDS_CONFIG\n");
        for (address, value) in &self.config {
            let _ = writeln!(header, "{address:08X} {value:08X}");
        }
        header.push_str("START_OF_TDR\n");
        out.write_all(header.as_bytes())?;
        out.write_all(&self.trace)
    }
}

/// The MCDS of a device.
#[derive(Debug, Clone, Copy)]
struct Layout {
    mcds_device: &'static str,
    device_name: &'static str,
    ctrl: u32,
    status: u32,
}

fn layout(jtag_id: u32) -> Result<Layout, McdsError> {
    match jtag_id & 0x0FFF_FFFF {
        // TC27x (part number 0x01DA), any revision.
        0x001D_A083 => Ok(Layout {
            mcds_device: "TC2XX_ED",
            device_name: "TC27xED",
            ctrl: TC2XX_ED_CTRL,
            status: TC2XX_ED_STATUS,
        }),
        _ => Err(McdsError::UnsupportedDevice(jtag_id)),
    }
}

/// The FIFO range that the configuration sets, and whether it writes the MCDS ID.
fn fifo_range(layout: &Layout, writes: &[(u32, u32)]) -> (u32, u32, bool) {
    let (mut base, mut top, mut id) = (0, 0, false);
    for &(address, value) in writes {
        let (_, address) = wire_address(address);
        if address == layout.ctrl + OFF_ID && value != 0 {
            id = true;
        } else if address == layout.status + OFF_FIFO_BASE {
            base = value & 0xFFFF_F000;
        } else if address == layout.status + OFF_FIFO_TOP {
            top = value;
        }
    }
    (base, top, id)
}

fn read_eec(bus: &mut dyn TriCoreBusAccess, address: u32) -> Result<u32, TriCoreError> {
    bus.read_u32(AddressMap::Eec, address)
}

/// Applies `writes` in order: the device registers in one batch, then the MCDS registers.
fn write_config(bus: &mut dyn TriCoreBusAccess, writes: &[(u32, u32)]) -> Result<(), McdsError> {
    let (mut target, mut eec) = (Vec::new(), Vec::new());
    for &(address, value) in writes {
        let (map, address) = wire_address(address);
        let transfer = BusTransfer::write_u32(map, address, value);
        match map {
            AddressMap::Target => target.push(transfer),
            _ => eec.push(transfer),
        }
    }
    for batch in [&mut target, &mut eec] {
        let limit = bus.limits().max_transfers.max(1);
        for chunk in batch.chunks_mut(limit) {
            bus.execute(chunk)?;
            chunk.iter().try_for_each(BusTransfer::check)?;
        }
    }
    Ok(())
}

/// A device word read between two readings of the MCDS timestamp counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamped {
    /// The counter before the read.
    pub before: u64,
    /// The word read.
    pub value: u32,
    /// The counter after the read.
    pub after: u64,
}

/// Reads the MCDS timestamp counter, which the TAS server offers in its own address map.
pub fn read_timestamp(bus: &mut dyn TriCoreBusAccess) -> Result<u64, McdsError> {
    let mut bytes = [0; 8];
    bus.read_block(AddressMap::Server, TIMESTAMP, &mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

/// Reads the word at `address` (map 0) between two readings of the MCDS timestamp counter, as
/// the Infineon recorder does. Each access is a request of its own, because a request may only
/// address one address map.
pub fn read_timestamped(
    bus: &mut dyn TriCoreBusAccess,
    address: u32,
) -> Result<Timestamped, McdsError> {
    let before = read_timestamp(bus)?;
    let value = bus.read_u32(AddressMap::Target, address)?;
    let after = read_timestamp(bus)?;
    Ok(Timestamped {
        before,
        value,
        after,
    })
}

/// Records a trace with `config`.
///
/// `progress` is called while the trace records. `started_at` gives the local time for the
/// header. The device must be an emulation device with an accessible EEC address map.
pub fn record(
    bus: &mut dyn TriCoreBusAccess,
    config: &McdsConfig,
    options: &RecordOptions,
    started_at: impl FnOnce() -> Timestamp,
    mut progress: impl FnMut(Progress),
) -> Result<Recording, McdsError> {
    let jtag_id = bus.jtag_id()?;
    let layout = layout(jtag_id)?;
    if let Some(config_id) = config.device_type
        && config_id & 0x0FFF_FFFF != jtag_id & 0x0FFF_FFFF
    {
        return Err(McdsError::WrongDevice {
            config: config_id,
            device: jtag_id,
        });
    }
    let (fifo_base, fifo_top, id_write) = fifo_range(&layout, &config.writes);
    let trace_max_kb = if fifo_top > fifo_base {
        (fifo_top - fifo_base + 1) >> 10
    } else {
        0
    };
    if trace_max_kb == 0 && !id_write {
        return Err(McdsError::NoEmemConfig);
    }
    let (start_writes, last) = if options.reset {
        let (&last, rest) = config.writes.split_last().ok_or(McdsError::EmptyConfig)?;
        if !CPU0_START.contains(&last.0) {
            return Err(McdsError::HarWithoutCpuStart(last.0));
        }
        (rest, Some(last))
    } else {
        (&config.writes[..], None)
    };

    // Only an emulation device has an EMEM.
    if bus.read_u32(AddressMap::Target, EMEM_PROBE).is_err() {
        return Err(McdsError::NotEmulationDevice);
    }
    if options.reset {
        bus.reset(ResetKind::ResetAndHalt)?;
    }
    // Nothing is written before the MCDS is known to be reachable and idle.
    match read_eec(bus, layout.ctrl + OFF_OCS) {
        Ok(0) => {}
        Ok(ocs) => return Err(McdsError::McdsBusy(ocs)),
        Err(error) => return Err(McdsError::McdsNotAccessible(error)),
    }

    let frequency_hz = bus
        .read_u32(AddressMap::Server, FREQUENCY_REGISTER)
        .ok()
        .filter(|&hz| hz >= FREQUENCY_MIN);

    // The TC27x ED ungates the auxiliary clock around the CPU start (reference sequence).
    bus.write_u32(AddressMap::Target, 0xFA00_6000, 0)?;
    if bus.read_u32(AddressMap::Target, 0xFA00_6000)? != 0 {
        bus.write_u32(AddressMap::Target, 0xF000_047C, 0xC0)?;
        bus.write_u32(AddressMap::Target, 0xFA00_6000, 0)?;
        bus.write_u32(AddressMap::Target, 0xF000_047C, 0x40)?;
    }

    let started_at = started_at();
    write_config(bus, start_writes)?;
    if let Some((address, value)) = last {
        let (map, address) = wire_address(address);
        bus.write_u32(map, address, value)?;
    }
    let start = Instant::now();

    let result = run_and_read_back(
        bus,
        &layout,
        fifo_base,
        trace_max_kb,
        options,
        start,
        &mut progress,
    );
    // Return the MCDS to idle whatever happened.
    let cleanup = match &options.cleanup {
        Some(cleanup) => write_config(bus, &cleanup.writes),
        None => Ok(()),
    };
    let (trace, elapsed) = result?;
    cleanup?;

    Ok(Recording {
        mcds_device: config
            .mcds_device
            .clone()
            .unwrap_or_else(|| layout.mcds_device.to_string()),
        device_type: jtag_id,
        device_name: layout.device_name.to_string(),
        frequency_hz,
        started_at,
        config: config.writes.clone(),
        trace,
        elapsed,
    })
}

/// Waits for the recording time, stops the trace and reads it back. Returns the trace and the
/// recording time.
fn run_and_read_back(
    bus: &mut dyn TriCoreBusAccess,
    layout: &Layout,
    fifo_base: u32,
    trace_max_kb: u32,
    options: &RecordOptions,
    start: Instant,
    progress: &mut impl FnMut(Progress),
) -> Result<(Vec<u8>, Duration), McdsError> {
    let fill_kb = |bus: &mut dyn TriCoreBusAccess| -> Result<u32, TriCoreError> {
        let fill = read_eec(bus, layout.status + OFF_FIFO_FILL)?;
        Ok(fill.saturating_sub(fifo_base).div_ceil(1024))
    };
    loop {
        let elapsed = start.elapsed();
        let trace_kb = fill_kb(bus)?;
        progress(Progress { trace_kb, elapsed });
        if elapsed >= options.duration {
            break;
        }
        std::thread::sleep(POLL.min(options.duration - elapsed));
    }

    bus.write_u32(AddressMap::Eec, layout.ctrl + OFF_TRACE_CTRL, TRACE_STOP)?;
    let elapsed = start.elapsed();
    let fill = fill_kb(bus)?;
    let size_kb = match read_eec(bus, layout.ctrl + OFF_TRACE_SIZE) {
        Ok(size) => ((size & 0xFFFF) * 0x20 + 0x4FF) >> 10,
        Err(_) => fill,
    };
    // The trace size plus the block overhead of 1/32.
    let mut read_kb = size_kb.div_ceil(32) + size_kb;
    if trace_max_kb != 0 {
        read_kb = read_kb.min(trace_max_kb);
    }
    if read_kb == 0 {
        return Err(McdsError::NoTraceData);
    }
    // The trace port delivers the next block on every read of the same address.
    let mut trace = vec![0; read_kb as usize * BLOCK];
    for block in trace.chunks_mut(BLOCK) {
        bus.read_block(AddressMap::Eec, layout.ctrl + OFF_TRACE_PORT, block)?;
    }
    Ok((trace, elapsed))
}

#[cfg(test)]
mod tests;
