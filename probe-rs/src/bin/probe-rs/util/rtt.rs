use crate::*;
use anyhow::{anyhow, Result};
use defmt_decoder::DecodeError;
pub use probe_rs::rtt::ChannelMode;
use probe_rs::rtt::{DownChannel, Rtt, ScanRegion, UpChannel};
use probe_rs::Core;
use probe_rs_target::MemoryRegion;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::File;
use std::{
    fmt,
    fmt::Write,
    fs,
    io::{Read, Seek},
    str::FromStr,
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
) -> Result<Option<Rtt>, anyhow::Error> {
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

/// Used by serde to provide defaults for `RttConfig`
fn default_channel_formats() -> Vec<RttChannelConfig> {
    vec![]
}

/// Used by serde to provide defaults for `RttChannelConfig::show_location`
fn default_include_location() -> bool {
    // Setting this to true to allow compatibility with behaviour prior to when this option was introduced.
    true
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum DataFormat {
    #[default]
    String,
    BinaryLE,
    Defmt,
}
impl FromStr for DataFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let src = s.to_ascii_lowercase();
        match &src.to_ascii_lowercase()[..] {
            // A forgiving/case-insensitive match
            "string" => Ok(Self::String),
            "binaryle" => Ok(Self::BinaryLE),
            "defmt" => Ok(Self::Defmt),
            _ => Err(format!("{src} is not a valid format")),
        }
    }
}

/// The initial configuration for RTT (Real Time Transfer). This configuration is complimented with the additional information specified for each of the channels in `RttChannel`.
#[derive(clap::Parser, Debug, Clone, Serialize, Deserialize, Default)]
pub struct RttConfig {
    #[structopt(skip)]
    #[serde(default, rename = "rttEnabled")]
    pub enabled: bool,
    /// Configure data_format and show_timestamps for select channels
    #[structopt(skip)]
    #[serde(default = "default_channel_formats", rename = "rttChannelFormats")]
    pub channels: Vec<RttChannelConfig>,
}

/// The User specified configuration for each active RTT Channel. The configuration is passed via a
/// DAP Client configuration (`launch.json`). If no configuration is specified, the defaults will be
/// `Dataformat::String` and `show_timestamps=false`.
#[derive(clap::Parser, Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RttChannelConfig {
    pub channel_number: Option<usize>,
    pub channel_name: Option<String>,
    #[serde(default)]
    pub data_format: DataFormat,
    #[structopt(skip)]
    #[serde(default)]
    // Control the inclusion of timestamps for DataFormat::String.
    pub show_timestamps: bool,
    #[structopt(skip)]
    #[serde(default = "default_include_location")]
    // Control the inclusion of source location information for DataFormat::Defmt.
    pub show_location: bool,
}

/// This is the primary interface through which RTT channel data is read and written. Every actual
/// RTT channel has a configuration and buffer that is used for this purpose.
#[derive(Debug)]
pub struct RttActiveChannel {
    pub up_channel: Option<UpChannel>,
    pub down_channel: Option<DownChannel>,
    pub channel_name: String,
    pub data_format: DataFormat,
    /// Data that will be written to the down_channel (host to target)
    _input_data: String,
    rtt_buffer: RttBuffer,
    show_timestamps: bool,
    show_location: bool,

    /// UTC offset used for creating timestamps
    ///
    /// Getting the offset can fail in multi-threaded programs,
    /// so it needs to be stored.
    timestamp_offset: UtcOffset,
}

/// A fully configured RttActiveChannel. The configuration will always try to 'default' based on
/// information read from the RTT control block in the binary. Where insufficient information is
/// available, it will use the supplied configuration, with final hardcoded defaults where no other
/// information was available.
impl RttActiveChannel {
    pub fn new(
        up_channel: Option<UpChannel>,
        down_channel: Option<DownChannel>,
        channel_config: Option<RttChannelConfig>,
        timestamp_offset: UtcOffset,
    ) -> Self {
        let full_config = match &channel_config {
            Some(channel_config) => channel_config.clone(),
            None => RttChannelConfig {
                ..Default::default() // Will set intelligent defaults below ...
            },
        };
        let buffer_size: usize = up_channel
            .as_ref()
            .map(|up| up.buffer_size())
            .or_else(|| down_channel.as_ref().map(|down| down.buffer_size()))
            .unwrap_or(1024); // If no explicit config is requested, assign a default
        let defmt_enabled: bool = up_channel
            .as_ref()
            .map(|up| up.name() == Some("defmt"))
            .or_else(|| {
                down_channel
                    .as_ref()
                    .map(|down| down.name() == Some("defmt"))
            })
            .unwrap_or(false); // If no explicit config is requested, assign a default
        let (data_format, show_location) = if defmt_enabled {
            let show_location = if let Some(channel_config) = channel_config {
                channel_config.show_location
            } else {
                true
            };
            (DataFormat::Defmt, show_location)
        } else {
            (full_config.data_format, false)
        };
        let name = up_channel
            .as_ref()
            .and_then(|up| up.name().map(Into::into))
            .or_else(|| {
                down_channel
                    .as_ref()
                    .and_then(|down| down.name().map(Into::into))
            })
            .or_else(|| full_config.clone().channel_name)
            .unwrap_or_else(|| {
                format!(
                    "Unnamed {:?} RTT channel - {}",
                    data_format,
                    full_config.channel_number.unwrap_or(0)
                )
            });
        Self {
            up_channel,
            down_channel,
            channel_name: name,
            data_format,
            _input_data: String::new(),
            rtt_buffer: RttBuffer::new(buffer_size),
            show_timestamps: full_config.show_timestamps,
            show_location,
            timestamp_offset,
        }
    }

    /// Returns the number of the `UpChannel`.
    pub fn number(&self) -> Option<usize> {
        self.up_channel.as_ref().map(|uc| uc.number())
    }

    /// Polls the RTT target for new data on the channel represented by `self`.
    /// Processes all the new data into the channel internal buffer and returns the number of bytes that was read.
    pub fn poll_rtt(&mut self, core: &mut Core) -> Option<usize> {
        if let Some(channel) = self.up_channel.as_mut() {
            // Retry loop, in case the probe is temporarily unavailable, e.g. user pressed the `reset` button.
            for _loop_count in 0..10 {
                match channel.read(core, self.rtt_buffer.0.as_mut()) {
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
        }
        None
    }

    /// Retrieves available data from the channel and if available, returns `Some(channel_number:String, formatted_data:String)`.
    /// If no data is available, or we encounter a recoverable error, it returns `None` value fore `formatted_data`.
    /// Non-recoverable errors are propagated to the caller.
    pub fn get_rtt_data(
        &mut self,
        core: &mut Core,
        defmt_state: Option<&DefmtState>,
    ) -> Result<Option<(String, String)>, anyhow::Error> {
        self.poll_rtt(core)
            .map(|bytes_read| {
                Ok((
                    self.number().unwrap_or(0).to_string(), // If the Channel doesn't have a number, then send the output to channel 0
                    {
                        let mut formatted_data = String::new();
                        match self.data_format {
                            DataFormat::String => self.get_string(bytes_read, &mut formatted_data),
                            DataFormat::BinaryLE => {
                                self.get_binary_le(bytes_read, &mut formatted_data)
                            }
                            DataFormat::Defmt => {
                                self.get_defmt(bytes_read, &mut formatted_data, defmt_state)?
                            }
                        };
                        formatted_data
                    },
                ))
            })
            .transpose()
    }

    pub fn _push_rtt(&mut self, core: &mut Core) {
        if let Some(down_channel) = self.down_channel.as_mut() {
            self._input_data += "\n";
            down_channel
                .write(core, self._input_data.as_bytes())
                .unwrap();
            self._input_data.clear();
        }
    }

    fn get_string(&self, bytes_read: usize, formatted_data: &mut String) {
        let incoming = String::from_utf8_lossy(&self.rtt_buffer.0[..bytes_read]).to_string();
        for line in incoming.split_terminator('\n') {
            if self.show_timestamps {
                write!(
                    formatted_data,
                    "{} :",
                    OffsetDateTime::now_utc().to_offset(self.timestamp_offset)
                )
                .expect("Writing to String cannot fail");
            }
            writeln!(formatted_data, "{line}").expect("Writing to String cannot fail");
        }
    }

    fn get_binary_le(&self, bytes_read: usize, formatted_data: &mut String) {
        for element in &self.rtt_buffer.0[..bytes_read] {
            // Width of 4 allows 0xFF to be printed.
            write!(formatted_data, "{element:#04x}").expect("Writing to String cannot fail");
        }
    }

    fn get_defmt(
        &self,
        bytes_read: usize,
        formatted_data: &mut String,
        defmt_state: Option<&DefmtState>,
    ) -> anyhow::Result<()> {
        match defmt_state {
            Some(DefmtState {
                table,
                locs,
                formatter,
            }) => {
                let mut stream_decoder = table.new_stream_decoder();
                stream_decoder.received(&self.rtt_buffer.0[..bytes_read]);
                loop {
                    match stream_decoder.decode() {
                        Ok(frame) => {
                            let loc = locs.as_ref().and_then(|locs| locs.get(&frame.index()));
                            let (file, line, module) = if let Some(loc) = loc {
                                let relpath = loc
                                    .file
                                    .strip_prefix(&std::env::current_dir().unwrap())
                                    .unwrap_or(&loc.file);
                                (
                                    Some(relpath.display().to_string()),
                                    Some(loc.line.try_into().unwrap()),
                                    Some(loc.module.as_str()),
                                )
                            } else {
                                (
                                    Some(format!(
                                        "└─ <invalid location: defmt frame-index: {}>",
                                        frame.index()
                                    )),
                                    None,
                                    None,
                                )
                            };
                            let s = formatter.format_frame(frame, file.as_deref(), line, module);
                            writeln!(formatted_data, "{s}").expect("Writing to String cannot fail");
                            continue;
                        }
                        Err(DecodeError::UnexpectedEof) => break,
                        Err(DecodeError::Malformed) => match table.encoding().can_recover() {
                            // If recovery is impossible, break out of here and propagate the error.
                            false => {
                                return Err(anyhow!("Unrecoverable error while decoding Defmt data and some data may have been lost: {:?}", DecodeError::Malformed));
                            }
                            // If recovery is possible, skip the current frame and continue with new data.
                            true => continue,
                        },
                    }
                }
            }
            None => {
                write!(
                    formatted_data,
                    "Running rtt in defmt mode but table or locations could not be loaded."
                )
                .expect("Writing to String cannot fail");
            }
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

pub struct DefmtState {
    table: defmt_decoder::Table,
    locs: Option<defmt_decoder::Locations>,
    formatter: defmt_decoder::log::format::Formatter,
}

impl fmt::Debug for DefmtState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DefmtState").finish()
    }
}

impl RttActiveTarget {
    /// RttActiveTarget collects references to all the `RttActiveChannel`s, for latter polling/pushing of data.
    pub fn new(
        mut rtt: probe_rs::rtt::Rtt,
        elf_file: &Path,
        rtt_config: &RttConfig,
        timestamp_offset: UtcOffset,
        log_format: Option<&str>,
    ) -> Result<Self> {
        let mut active_channels = Vec::new();
        // For each channel configured in the RTT Control Block (`Rtt`), check if there are additional user configuration in a `RttChannelConfig`. If not, apply defaults.
        let up_channels = rtt.up_channels().drain();
        let down_channels = rtt.down_channels().drain();
        for channel in up_channels {
            let number = channel.number();
            let channel_config = rtt_config
                .channels
                .clone()
                .into_iter()
                .find(|channel| channel.channel_number == Some(number));
            active_channels.push(RttActiveChannel::new(
                Some(channel),
                None,
                channel_config,
                timestamp_offset,
            ));
        }

        for channel in down_channels {
            let number = channel.number();
            let channel_config = rtt_config
                .channels
                .clone()
                .into_iter()
                .find(|channel| channel.channel_number == Some(number));
            active_channels.push(RttActiveChannel::new(
                None,
                Some(channel),
                channel_config,
                timestamp_offset,
            ));
        }

        // It doesn't make sense to pretend RTT is active, if there are no active channels
        if active_channels.is_empty() {
            return Err(anyhow!(
                "RTT Initialized correctly, but there were no active channels configured"
            ));
        }

        let defmt_enabled = active_channels
            .iter()
            .any(|elem| elem.data_format == DataFormat::Defmt);
        let defmt_state = if defmt_enabled {
            let elf = fs::read(elf_file).map_err(|err| {
                anyhow!(
                    "Error reading program binary while initalizing RTT: {}",
                    err
                )
            })?;

            let show_location = active_channels
                .first()
                .expect("`active_channels` is not empty")
                .show_location;

            if let Some(table) = defmt_decoder::Table::parse(&elf)? {
                let has_timestamp = table.has_timestamp();

                // Format options:
                // 1. Custom format
                // 2. Default with timestamp with location
                // 3. Default with timestamp without location
                // 4. Default without timestamp with location
                // 5. Default without timestamp without location
                let format = log_format.unwrap_or(match (show_location, has_timestamp) {
                    (true, true) => "{t} {L} {s}\n└─ {m} @ {F}:{l}",
                    (true, false) => "{L} {s}\n└─ {m} @ {F}:{l}",
                    (false, true) => "{t} {L} {s}",
                    (false, false) => "{L} {s}",
                });
                let format = defmt_decoder::log::format::FormatterConfig::custom(format);
                let formatter = defmt_decoder::log::format::Formatter::new(format);

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
                Some(DefmtState {
                    table,
                    locs,
                    formatter,
                })
            } else {
                tracing::warn!("No `Table` definition in DWARF info; compile your program with `debug = 2` to enable location info.");
                None
            }
        } else {
            None
        };

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
                    if let Some(name) = binary.strtab.get_at(sym.st_name) {
                        if name == "_SEGGER_RTT" {
                            return Some(sym.st_value);
                        }
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
    ) -> Result<HashMap<String, String>, anyhow::Error> {
        let defmt_state = self.defmt_state.as_ref();
        let mut data = HashMap::new();
        for channel in self.active_channels.iter_mut() {
            if let Some((channel, formatted_data)) = channel.get_rtt_data(core, defmt_state)? {
                data.insert(channel, formatted_data);
            }
        }
        Ok(data)
    }

    // pub fn push_rtt(&mut self) {
    //     self.tabs[self.current_tab].push_rtt();
    // }
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
