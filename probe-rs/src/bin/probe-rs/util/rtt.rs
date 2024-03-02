use anyhow::{anyhow, Result};
use defmt_decoder::DecodeError;
pub use probe_rs::rtt::ChannelMode;
use probe_rs::rtt::{DownChannel, Error, Rtt, ScanRegion, UpChannel};
use probe_rs::Core;
use probe_rs_target::MemoryRegion;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::{
    fmt,
    fmt::Write,
    fs,
    io::{Read, Seek},
    path::Path,
};
use time::{OffsetDateTime, UtcOffset};

/// Try to find the RTT control block in the ELF file and attach to it.
///
/// This function can return `Ok(None)` to indicate that RTT is not available on the target.
pub fn attach_to_rtt(
    core: &mut Core,
    memory_map: &[MemoryRegion],
    rtt_region: &ScanRegion,
    elf_file: &Path,
) -> Result<Option<Rtt>> {
    // Try to find the RTT control block symbol in the ELF file.

    // If we find it, we can use the exact address to attach to the RTT control block. Otherwise, we
    // fall back to the caller-provided scan regions.
    let exact_rtt_region;
    let mut rtt_region = rtt_region;

    if let Ok(mut file) = File::open(elf_file) {
        if let Some(address) = RttActiveTarget::get_rtt_symbol(&mut file) {
            exact_rtt_region = ScanRegion::Exact(address as u32);
            rtt_region = &exact_rtt_region;
        }
    }

    tracing::info!("Initializing RTT");

    if let ScanRegion::Ranges(rngs) = &rtt_region {
        if rngs.is_empty() {
            // We have no regions to scan so we cannot initialize RTT.
            tracing::debug!("ELF file has no RTT block symbol, and this target does not support automatic scanning");
            return Ok(None);
        }
    }

    match Rtt::attach_region(core, memory_map, rtt_region) {
        Ok(rtt) => {
            tracing::info!("RTT initialized.");
            Ok(Some(rtt))
        }
        Err(err) => Err(anyhow!("Error attempting to attach to RTT: {}", err)),
    }
}

/// Used by serde to provide defaults for `RttChannelConfig::show_location`
fn default_include_location() -> bool {
    // Setting this to true to allow compatibility with behaviour prior to when this option was introduced.
    true
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DataFormat {
    #[default]
    String,
    BinaryLE,
    Defmt,
}

/// The initial configuration for RTT (Real Time Transfer). This configuration is complimented with the additional information specified for each of the channels in `RttChannel`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RttConfig {
    #[serde(default, rename = "rttEnabled")]
    pub enabled: bool,

    /// The default format string to use for decoding defmt logs.
    #[serde(default, rename = "defmtLogFormat")]
    pub log_format: Option<String>,

    /// Configure data_format and show_timestamps for select channels
    #[serde(default = "Vec::new", rename = "rttChannelFormats")]
    pub channels: Vec<RttChannelConfig>,
}

/// The User specified configuration for each active RTT Channel. The configuration is passed via a
/// DAP Client configuration (`launch.json`). If no configuration is specified, the defaults will be
/// `DataFormat::String` and `show_timestamps=false`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RttChannelConfig {
    pub channel_number: Option<usize>,
    pub channel_name: Option<String>,
    #[serde(default)]
    pub data_format: DataFormat,

    #[serde(default)]
    // Control the inclusion of timestamps for DataFormat::String.
    pub show_timestamps: bool,

    #[serde(default = "default_include_location")]
    // Control the inclusion of source location information for DataFormat::Defmt.
    pub show_location: bool,

    #[serde(default)]
    // Control the output format for DataFormat::Defmt.
    pub defmt_log_format: Option<String>,
}

pub enum ChannelDataConfig {
    String {
        show_timestamps: bool,
        last_line_done: bool,
    },
    BinaryLE,
    Defmt {
        formatter: defmt_decoder::log::format::Formatter,
    },
}

impl std::fmt::Debug for ChannelDataConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelDataConfig::String {
                show_timestamps,
                last_line_done,
            } => f
                .debug_struct("String")
                .field("show_timestamps", show_timestamps)
                .field("last_line_done", last_line_done)
                .finish(),
            ChannelDataConfig::BinaryLE => f.debug_struct("BinaryLE").finish(),
            ChannelDataConfig::Defmt { .. } => f.debug_struct("Defmt").finish_non_exhaustive(),
        }
    }
}

pub trait ChannelDataCallbacks {
    fn on_binary_data(&mut self, channel: usize, data: &[u8]) -> Result<()> {
        let mut formatted_data = String::with_capacity(data.len() * 4);
        for element in data {
            // Width of 4 allows 0xFF to be printed.
            write!(&mut formatted_data, "{element:#04x}").expect("Writing to String cannot fail");
        }
        self.on_string_data(channel, formatted_data)
    }

    fn on_string_data(&mut self, channel: usize, data: String) -> Result<()>;
}

#[derive(Debug)]
pub struct RttActiveUpChannel {
    pub up_channel: UpChannel,
    pub channel_name: String,
    pub data_format: ChannelDataConfig,
    rtt_buffer: RttBuffer,

    /// UTC offset used for creating timestamps
    ///
    /// Getting the offset can fail in multi-threaded programs,
    /// so it needs to be stored.
    timestamp_offset: UtcOffset,
}

impl RttActiveUpChannel {
    pub fn new(
        up_channel: UpChannel,
        rtt_config: &RttConfig,
        channel_config: &RttChannelConfig,
        timestamp_offset: UtcOffset,
        defmt_state: Option<&DefmtState>,
    ) -> Self {
        let buffer_size = up_channel.buffer_size();
        let defmt_enabled = up_channel.name() == Some("defmt");

        let data_format = match channel_config.data_format {
            DataFormat::String if !defmt_enabled => ChannelDataConfig::String {
                show_timestamps: channel_config.show_timestamps,
                last_line_done: true,
            },

            DataFormat::BinaryLE if !defmt_enabled => ChannelDataConfig::BinaryLE,

            _ => {
                let has_timestamp = if let Some(defmt) = defmt_state {
                    defmt.table.has_timestamp()
                } else {
                    tracing::warn!("No `Table` definition in DWARF info; compile your program with `debug = 2` to enable location info.");
                    false
                };

                // Format options:
                // 1. Custom format for the channel
                // 2. Custom default format
                // 3. Default with timestamp with location
                // 4. Default with timestamp without location
                // 5. Default without timestamp with location
                // 6. Default without timestamp without location
                let format = channel_config
                    .defmt_log_format
                    .as_deref()
                    .or(rtt_config.log_format.as_deref())
                    .unwrap_or(match (channel_config.show_location, has_timestamp) {
                        (true, true) => "{t} {L} {s}\n└─ {m} @ {F}:{l}",
                        (true, false) => "{L} {s}\n└─ {m} @ {F}:{l}",
                        (false, true) => "{t} {L} {s}",
                        (false, false) => "{L} {s}",
                    });
                let mut format = defmt_decoder::log::format::FormatterConfig::custom(format);
                format.is_timestamp_available = has_timestamp;
                let formatter = defmt_decoder::log::format::Formatter::new(format);
                ChannelDataConfig::Defmt { formatter }
            }
        };

        let channel_name = up_channel
            .name()
            .or(channel_config.channel_name.as_deref())
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                format!(
                    "Unnamed {:?} RTT channel - {}",
                    channel_config.data_format,
                    up_channel.number()
                )
            });

        Self {
            up_channel,
            channel_name,
            data_format,
            rtt_buffer: RttBuffer::new(buffer_size),
            timestamp_offset,
        }
    }

    pub fn number(&self) -> usize {
        self.up_channel.number()
    }

    /// Polls the RTT target for new data on the channel represented by `self`.
    /// Processes all the new data into the channel internal buffer and returns the number of bytes that was read.
    pub fn poll_rtt(&mut self, core: &mut Core) -> Option<usize> {
        // Retry loop, in case the probe is temporarily unavailable, e.g. user pressed the `reset` button.
        for _loop_count in 0..10 {
            match self.up_channel.read(core, self.rtt_buffer.0.as_mut()) {
                Ok(0) => return None,
                Ok(count) => return Some(count),
                Err(probe_rs::rtt::Error::Probe(_)) => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(err) => {
                    tracing::error!("\nError reading from RTT: {}", err);
                    return None;
                }
            }
        }

        None
    }

    /// Retrieves available data from the channel and if available, returns `Some(channel_number:String, formatted_data:String)`.
    /// If no data is available, or we encounter a recoverable error, it returns `None` value for `formatted_data`.
    /// Non-recoverable errors are propagated to the caller.
    pub fn poll_process_rtt_data(
        &mut self,
        core: &mut Core,
        defmt_state: Option<&DefmtState>,
        collector: &mut impl ChannelDataCallbacks,
    ) -> Result<()> {
        let Some(bytes_read) = self.poll_rtt(core) else {
            return Ok(());
        };

        let buffer = &self.rtt_buffer.0[..bytes_read];

        match self.data_format {
            ChannelDataConfig::BinaryLE => collector.on_binary_data(self.number(), buffer),
            ChannelDataConfig::String {
                show_timestamps,
                ref mut last_line_done,
            } => {
                let timestamp = show_timestamps
                    .then(|| OffsetDateTime::now_utc().to_offset(self.timestamp_offset));

                let string = Self::process_string(buffer, timestamp, last_line_done);
                collector.on_string_data(self.number(), string)
            }
            ChannelDataConfig::Defmt { ref formatter } => {
                let string = self.process_defmt(buffer, defmt_state, formatter)?;
                collector.on_string_data(self.number(), string)
            }
        }
    }

    fn process_string(
        buffer: &[u8],
        timestamp: Option<OffsetDateTime>,
        last_line_done: &mut bool,
    ) -> String {
        let incoming = String::from_utf8_lossy(buffer);
        if let Some(timestamp) = timestamp {
            let mut formatted_data = String::new();
            for line in incoming.split_inclusive('\n') {
                if *last_line_done {
                    write!(formatted_data, "{timestamp}: ").expect("Writing to String cannot fail");
                }
                writeln!(formatted_data, "{line}").expect("Writing to String cannot fail");
                *last_line_done = line.ends_with('\n');
            }
            formatted_data
        } else {
            incoming.to_string()
        }
    }

    fn process_defmt(
        &self,
        buffer: &[u8],
        defmt_state: Option<&DefmtState>,
        formatter: &defmt_decoder::log::format::Formatter,
    ) -> Result<String> {
        let Some(DefmtState { table, locs }) = defmt_state else {
            return Ok(String::from(
                "Trying to process defmt data but table or locations could not be loaded.\n",
            ));
        };

        let mut stream_decoder = table.new_stream_decoder();
        stream_decoder.received(buffer);
        let current_dir = std::env::current_dir().unwrap();

        let mut formatted_data = String::new();
        loop {
            match stream_decoder.decode() {
                Ok(frame) => {
                    let loc = locs.as_ref().and_then(|locs| locs.get(&frame.index()));
                    let (file, line, module) = if let Some(loc) = loc {
                        let relpath = loc.file.strip_prefix(&current_dir).unwrap_or(&loc.file);
                        (
                            relpath.display().to_string(),
                            Some(loc.line.try_into().unwrap()),
                            Some(loc.module.as_str()),
                        )
                    } else {
                        (
                            format!(
                                "└─ <invalid location: defmt frame-index: {}>",
                                frame.index()
                            ),
                            None,
                            None,
                        )
                    };
                    let s = formatter.format_frame(frame, Some(&file), line, module);
                    writeln!(formatted_data, "{s}").expect("Writing to String cannot fail");
                    continue;
                }
                Err(DecodeError::UnexpectedEof) => break,
                Err(DecodeError::Malformed) if table.encoding().can_recover() => {
                    // If recovery is possible, skip the current frame and continue with new data.
                    continue;
                }
                Err(DecodeError::Malformed) => {
                    return Err(anyhow!(
                        "Unrecoverable error while decoding Defmt \
                        data and some data may have been lost: {:?}",
                        DecodeError::Malformed
                    ));
                }
            }
        }

        Ok(formatted_data)
    }

    pub(crate) fn set_mode(
        &self,
        core: &mut Core<'_>,
        block_if_full: ChannelMode,
    ) -> Result<(), Error> {
        self.up_channel.set_mode(core, block_if_full)
    }
}

#[derive(Debug)]
pub struct RttActiveDownChannel {
    pub down_channel: DownChannel,
    pub channel_name: String,
    /// Data that will be written to the down_channel (host to target)
    _input_data: String,
}

impl RttActiveDownChannel {
    pub fn new(down_channel: DownChannel, channel_config: &RttChannelConfig) -> Self {
        let channel_name = down_channel
            .name()
            .or(channel_config.channel_name.as_deref())
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("Unnamed RTT down channel - {}", down_channel.number()));

        Self {
            down_channel,
            channel_name,
            _input_data: String::new(),
        }
    }

    pub fn _input(&self) -> &str {
        self._input_data.as_ref()
    }

    pub fn _input_mut(&mut self) -> &mut String {
        &mut self._input_data
    }

    fn _push_rtt(&mut self, core: &mut Core<'_>) -> Result<(), Error> {
        self._input_data += "\n";
        let result = self
            .down_channel
            .write(core, self._input_data.as_bytes())
            .map(|_| ());
        self._input_data.clear();
        result
    }
}

/// This is the primary interface through which RTT channel data is read and written. Every actual
/// RTT channel has a configuration and buffer that is used for this purpose.
#[derive(Debug)]
pub struct RttActiveChannel {
    pub data_format: DataFormat,
    pub up_channel: Option<RttActiveUpChannel>,
    pub down_channel: Option<RttActiveDownChannel>,
}

/// A fully configured RttActiveChannel. The configuration will always try to 'default' based on
/// information read from the RTT control block in the binary. Where insufficient information is
/// available, it will use the supplied configuration, with final hardcoded defaults where no other
/// information was available.
impl RttActiveChannel {
    fn new(
        number: usize,
        up_channel: Option<UpChannel>,
        down_channel: Option<DownChannel>,
        rtt_config: &RttConfig,
        timestamp_offset: UtcOffset,
        defmt_state: Option<&DefmtState>,
    ) -> Self {
        let channel_config = rtt_config
            .channels
            .iter()
            .find(|channel| channel.channel_number == Some(number))
            .cloned()
            .unwrap_or_default();

        Self {
            data_format: channel_config.data_format,
            up_channel: up_channel.map(|channel| {
                RttActiveUpChannel::new(
                    channel,
                    rtt_config,
                    &channel_config,
                    timestamp_offset,
                    defmt_state,
                )
            }),
            down_channel: down_channel
                .map(|channel| RttActiveDownChannel::new(channel, &channel_config)),
        }
    }

    /// Returns the number of the `UpChannel`.
    pub fn number(&self) -> Option<usize> {
        self.up_channel.as_ref().map(|uc| uc.number())
    }

    /// Retrieves available data from the channel and if available.
    /// If no data is available, or we encounter a recoverable error, it returns `None` value fore `formatted_data`.
    /// Non-recoverable errors are propagated to the caller.
    pub fn get_rtt_data(
        &mut self,
        core: &mut Core,
        defmt_state: Option<&DefmtState>,
        collector: &mut impl ChannelDataCallbacks,
    ) -> Result<()> {
        if let Some(up_channel) = self.up_channel.as_mut() {
            up_channel.poll_process_rtt_data(core, defmt_state, collector)
        } else {
            Ok(())
        }
    }

    pub fn _push_rtt(&mut self, core: &mut Core) -> Result<()> {
        if let Some(down_channel) = self.down_channel.as_mut() {
            down_channel._push_rtt(core)?;
        }

        Ok(())
    }
}

/// Once an active connection with the Target RTT control block has been established, we configure
/// each of the active channels, and hold essential state information for successful communication.
#[derive(Debug)]
pub struct RttActiveTarget {
    pub active_channels: Vec<RttActiveChannel>,
    pub defmt_state: Option<DefmtState>,
}

/// defmt information common to all defmt channels.
pub struct DefmtState {
    table: defmt_decoder::Table,
    locs: Option<defmt_decoder::Locations>,
}

impl fmt::Debug for DefmtState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DefmtState").finish_non_exhaustive()
    }
}

impl RttActiveTarget {
    /// RttActiveTarget collects references to all the `RttActiveChannel`s, for latter polling/pushing of data.
    pub fn new(
        rtt: probe_rs::rtt::Rtt,
        elf_file: &Path,
        rtt_config: &RttConfig,
        timestamp_offset: UtcOffset,
    ) -> Result<Self> {
        let defmt_state = {
            let elf = fs::read(elf_file).map_err(|err| {
                anyhow!(
                    "Error reading program binary while initalizing RTT: {}",
                    err
                )
            })?;

            if let Some(table) = defmt_decoder::Table::parse(&elf)? {
                let locs = {
                    let locs = table.get_locations(&elf)?;

                    if !table.is_empty() && locs.is_empty() {
                        tracing::warn!("Insufficient DWARF info; compile your program with `debug = 2` to enable location info.");
                        None
                    } else if table.indices().all(|idx| locs.contains_key(&(idx as u64))) {
                        Some(locs)
                    } else {
                        tracing::warn!(
                            "Location info is incomplete; it will be omitted from the output."
                        );
                        None
                    }
                };
                Some(DefmtState { table, locs })
            } else {
                None
            }
        };

        let mut active_channels = Vec::new();
        // For each channel configured in the RTT Control Block (`Rtt`), check if there are additional user configuration in a `RttChannelConfig`. If not, apply defaults.
        for channel in rtt.up_channels.into_iter() {
            let number = channel.number();
            active_channels.push(RttActiveChannel::new(
                number,
                Some(channel),
                None,
                rtt_config,
                timestamp_offset,
                defmt_state.as_ref(),
            ));
        }

        for channel in rtt.down_channels.into_iter() {
            let number = channel.number();
            active_channels.push(RttActiveChannel::new(
                number,
                None,
                Some(channel),
                rtt_config,
                timestamp_offset,
                defmt_state.as_ref(),
            ));
        }

        // It doesn't make sense to pretend RTT is active, if there are no active channels
        if active_channels.is_empty() {
            return Err(anyhow!(
                "RTT Initialized correctly, but there were no active channels configured"
            ));
        }

        Ok(Self {
            active_channels,
            defmt_state,
        })
    }

    pub fn get_rtt_symbol<T: Read + Seek>(file: &mut T) -> Option<u64> {
        let mut buffer = Vec::new();
        if file.read_to_end(&mut buffer).is_ok() {
            if let Ok(binary) = goblin::elf::Elf::parse(buffer.as_slice()) {
                for sym in &binary.syms {
                    if binary.strtab.get_at(sym.st_name) == Some("_SEGGER_RTT") {
                        return Some(sym.st_value);
                    }
                }
            }
        }

        tracing::warn!(
            "No RTT header info was present in the ELF file. Does your firmware run RTT?"
        );
        None
    }

    /// Polls the RTT target on all channels and returns available data.
    /// An error on any channel will return an error instead of incomplete data.
    pub fn poll_rtt_fallible(
        &mut self,
        core: &mut Core,
        collector: &mut impl ChannelDataCallbacks,
    ) -> Result<()> {
        let defmt_state = self.defmt_state.as_ref();
        for channel in self.active_channels.iter_mut() {
            channel.get_rtt_data(core, defmt_state, collector)?;
        }
        Ok(())
    }
}

pub(crate) struct RttBuffer(pub Vec<u8>);
impl RttBuffer {
    /// Initialize the buffer and ensure it has enough capacity to match the size of the RTT channel on the target at the time of instantiation. Doing this now prevents later performance impact if the buffer capacity has to be grown dynamically.
    pub fn new(buffer_size: usize) -> RttBuffer {
        let rtt_buffer = vec![0u8; buffer_size.max(1)];
        RttBuffer(rtt_buffer)
    }
}
impl fmt::Debug for RttBuffer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}
