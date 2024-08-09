use anyhow::{anyhow, Context};
use defmt_decoder::log::format::{Formatter, FormatterConfig, FormatterFormat};
use defmt_decoder::DecodeError;
pub use probe_rs::rtt::ChannelMode;
use probe_rs::rtt::{DownChannel, Error, Rtt, UpChannel};
use probe_rs::{Core, MemoryInterface, Session};
use probe_rs_target::MemoryRegion;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::{
    fmt,
    fmt::Write,
    io::{Read, Seek},
    path::Path,
};
use time::{macros::format_description, OffsetDateTime, UtcOffset};

pub(crate) mod client;

/// Infer the target core from the RTT symbol. Useful for multi-core targets.
pub fn get_target_core_id(session: &mut Session, elf_file: impl AsRef<Path>) -> usize {
    let maybe_core_id = || {
        let mut file = File::open(elf_file).ok()?;
        let address = RttActiveTarget::get_rtt_symbol(&mut file)?;

        tracing::debug!("RTT symbol found at {address:#010x}");

        let target_memory = session
            .target()
            .memory_map
            .iter()
            .filter_map(MemoryRegion::as_ram_region)
            .find(|region| region.range.contains(&address))?;

        tracing::debug!("RTT symbol is in RAM region {:?}", target_memory.name);

        let core_name = target_memory.cores.first()?;
        let core_id = session
            .target()
            .cores
            .iter()
            .position(|core| core.name == *core_name)?;

        tracing::debug!("RTT symbol is in core {core_id}");

        Some(core_id)
    };
    maybe_core_id().unwrap_or(0)
}

/// Used by serde to provide defaults for `RttChannelConfig::show_timestamps`
fn default_show_timestamps() -> bool {
    true
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Default, docsplay::Display)]
pub enum DataFormat {
    #[default]
    /// string
    String,
    /// binary
    BinaryLE,
    /// defmt
    Defmt,
}

/// The initial configuration for RTT (Real Time Transfer). This configuration is complimented with the additional information specified for each of the channels in `RttChannel`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RttConfig {
    #[serde(default, rename = "rttEnabled")]
    pub enabled: bool,

    /// Configure data_format and show_timestamps for select channels
    #[serde(default = "Vec::new", rename = "rttChannelFormats")]
    pub channels: Vec<RttChannelConfig>,
}

impl RttConfig {
    /// Returns the configuration for the specified channel number, if it exists.
    pub fn channel_config(&self, channel_number: usize) -> Option<&RttChannelConfig> {
        self.channels
            .iter()
            .find(|ch| ch.channel_number == Some(channel_number))
    }
}

/// The User specified configuration for each active RTT Channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RttChannelConfig {
    pub channel_number: Option<usize>,
    #[serde(default)]
    pub data_format: DataFormat,

    /// RTT channel operating mode. Defaults to the target's configuration.
    #[serde(default)]
    pub mode: Option<ChannelMode>,

    #[serde(default = "default_show_timestamps")]
    /// Controls the inclusion of timestamps for [`DataFormat::String`] and [`DataFormat::Defmt`].
    pub show_timestamps: bool,

    #[serde(default)]
    /// Controls the inclusion of source location information for DataFormat::Defmt.
    pub show_location: bool,

    #[serde(default)]
    /// Controls the output format for DataFormat::Defmt.
    pub log_format: Option<String>,
}

impl Default for RttChannelConfig {
    fn default() -> Self {
        RttChannelConfig {
            channel_number: Default::default(),
            data_format: Default::default(),
            mode: Default::default(),
            show_timestamps: default_show_timestamps(),
            show_location: Default::default(),
            log_format: Default::default(),
        }
    }
}

pub enum ChannelDataFormat {
    String {
        /// UTC offset used for creating timestamps, if enabled.
        ///
        /// Getting the offset can fail in multi-threaded programs,
        /// so it needs to be stored.
        timestamp_offset: Option<UtcOffset>,
        last_line_done: bool,
    },
    BinaryLE,
    Defmt {
        formatter: Formatter,
        // CWD to strip from file paths in defmt output
        cwd: PathBuf,
        defmt_data: Option<Arc<DefmtState>>,
    },
}

impl From<&ChannelDataFormat> for DataFormat {
    fn from(config: &ChannelDataFormat) -> Self {
        match config {
            ChannelDataFormat::String { .. } => DataFormat::String,
            ChannelDataFormat::BinaryLE => DataFormat::BinaryLE,
            ChannelDataFormat::Defmt { .. } => DataFormat::Defmt,
        }
    }
}

impl fmt::Debug for ChannelDataFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChannelDataFormat::String {
                timestamp_offset,
                last_line_done,
            } => f
                .debug_struct("String")
                .field("timestamp_offset", timestamp_offset)
                .field("last_line_done", last_line_done)
                .finish(),
            ChannelDataFormat::BinaryLE => f.debug_struct("BinaryLE").finish(),
            ChannelDataFormat::Defmt { .. } => f.debug_struct("Defmt").finish_non_exhaustive(),
        }
    }
}

impl ChannelDataFormat {
    /// Returns whether the channel is expected to output binary data (`true`)
    /// or human-readable strings (`false`).
    pub fn is_binary(&self) -> bool {
        matches!(self, ChannelDataFormat::BinaryLE)
    }

    fn process(
        &mut self,
        number: usize,
        buffer: &[u8],
        collector: &mut impl ChannelDataCallbacks,
    ) -> Result<(), Error> {
        // FIXME: clean this up by splitting the enum variants out into separate structs
        match self {
            ChannelDataFormat::BinaryLE => collector.on_binary_data(number, buffer),
            ChannelDataFormat::String {
                timestamp_offset,
                ref mut last_line_done,
            } => {
                let string = Self::process_string(buffer, *timestamp_offset, last_line_done)?;
                collector.on_string_data(number, string)
            }
            ChannelDataFormat::Defmt {
                ref formatter,
                ref cwd,
                ref defmt_data,
            } => {
                let string = Self::process_defmt(buffer, defmt_data.as_deref(), formatter, cwd)?;
                collector.on_string_data(number, string)
            }
        }
    }

    fn process_string(
        buffer: &[u8],
        offset: Option<UtcOffset>,
        last_line_done: &mut bool,
    ) -> Result<String, Error> {
        let incoming = String::from_utf8_lossy(buffer);

        let Some(offset) = offset else {
            return Ok(incoming.to_string());
        };

        let timestamp = OffsetDateTime::now_utc()
            .to_offset(offset)
            .format(format_description!(
                "[hour repr:24]:[minute]:[second].[subsecond digits:3]"
            ))
            .expect("Incorrect format string. This shouldn't happen.");

        let mut formatted_data = String::new();
        for line in incoming.split_inclusive('\n') {
            if *last_line_done {
                write!(formatted_data, "{timestamp}: ").expect("Writing to String cannot fail");
            }
            write!(formatted_data, "{line}").expect("Writing to String cannot fail");
            *last_line_done = line.ends_with('\n');
        }
        Ok(formatted_data)
    }

    fn process_defmt(
        buffer: &[u8],
        defmt_state: Option<&DefmtState>,
        formatter: &Formatter,
        cwd: &Path,
    ) -> Result<String, Error> {
        let Some(DefmtState { table, locs }) = defmt_state else {
            return Ok(String::from(
                "Trying to process defmt data but table or locations could not be loaded.\n",
            ));
        };

        let mut stream_decoder = table.new_stream_decoder();

        // FIXME: this assumes we read frames atomically which is implementation-defined and we
        // should be able to handle the case where a frame is split across two reads with a
        // temporary buffer.
        stream_decoder.received(buffer);

        let mut formatted_data = String::new();
        loop {
            match stream_decoder.decode() {
                Ok(frame) => {
                    let loc = locs.as_ref().and_then(|locs| locs.get(&frame.index()));
                    let (file, line, module) = if let Some(loc) = loc {
                        let relpath = loc.file.strip_prefix(cwd).unwrap_or(&loc.file);
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
                }
                Err(DecodeError::UnexpectedEof) => break,
                Err(DecodeError::Malformed) if table.encoding().can_recover() => {
                    // If recovery is possible, skip the current frame and continue with new data.
                }
                Err(DecodeError::Malformed) => {
                    return Err(Error::Other(anyhow!(
                        "Unrecoverable error while decoding Defmt \
                        data. Some data may have been lost: {}",
                        DecodeError::Malformed
                    )));
                }
            }
        }

        Ok(formatted_data)
    }
}

pub trait ChannelDataCallbacks {
    fn on_binary_data(&mut self, channel: usize, data: &[u8]) -> Result<(), Error> {
        let mut formatted_data = String::with_capacity(data.len() * 4);
        for element in data {
            // Width of 4 allows 0xFF to be printed.
            write!(&mut formatted_data, "{element:#04x}").expect("Writing to String cannot fail");
        }
        self.on_string_data(channel, formatted_data)
    }

    fn on_string_data(&mut self, channel: usize, data: String) -> Result<(), Error>;
}

#[derive(Debug)]
pub struct RttActiveUpChannel {
    pub up_channel: UpChannel,
    pub data_format: ChannelDataFormat,
    rtt_buffer: Box<[u8]>,

    /// If set, the original mode of the channel before we changed it. Upon exit we should do
    /// our best to restore the original mode.
    original_mode: Option<ChannelMode>,
}

impl RttActiveUpChannel {
    pub fn new(
        core: &mut Core,
        up_channel: UpChannel,
        channel_config: &RttChannelConfig,
        timestamp_offset: UtcOffset,
        defmt_data: Option<Arc<DefmtState>>,
    ) -> Result<Self, Error> {
        let is_defmt_channel = up_channel.name() == Some("defmt");

        let data_format = match channel_config.data_format {
            DataFormat::String if !is_defmt_channel => ChannelDataFormat::String {
                timestamp_offset: channel_config.show_timestamps.then_some(timestamp_offset),
                last_line_done: true,
            },

            DataFormat::BinaryLE if !is_defmt_channel => ChannelDataFormat::BinaryLE,

            // either DataFormat::Defmt is configured, or defmt_enabled is true
            _ => {
                let has_timestamp = if let Some(ref defmt) = defmt_data {
                    defmt.table.has_timestamp()
                } else {
                    tracing::warn!("No `Table` definition in DWARF info; compile your program with `debug = 2` to enable location info.");
                    false
                };

                // Format options:
                // 1. Custom format for the channel
                // 2. Default with optional timestamp and location
                let format = if let Some(format) = channel_config.log_format.as_deref() {
                    FormatterFormat::Custom(format)
                } else {
                    FormatterFormat::Default {
                        with_location: channel_config.show_location,
                    }
                };

                ChannelDataFormat::Defmt {
                    formatter: Formatter::new(FormatterConfig {
                        format,
                        is_timestamp_available: has_timestamp && channel_config.show_timestamps,
                    }),
                    cwd: std::env::current_dir().unwrap(),
                    defmt_data,
                }
            }
        };

        let mut original_mode = None;
        if let Some(mode) = channel_config.mode.or(
            // Try not to corrupt the byte stream if using defmt
            if matches!(data_format, ChannelDataFormat::Defmt { .. }) {
                Some(ChannelMode::BlockIfFull)
            } else {
                None
            },
        ) {
            original_mode = Some(up_channel.mode(core)?);
            up_channel.set_mode(core, mode)?;
        }

        Ok(Self {
            rtt_buffer: vec![0; up_channel.buffer_size().max(1)].into_boxed_slice(),
            up_channel,
            data_format,
            original_mode,
        })
    }

    pub fn channel_name(&self) -> String {
        self.up_channel
            .name()
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                format!(
                    "Unnamed {} RTT up channel - {}",
                    DataFormat::from(&self.data_format),
                    self.up_channel.number()
                )
            })
    }

    pub fn number(&self) -> usize {
        self.up_channel.number()
    }

    /// Polls the RTT target for new data on the channel represented by `self`.
    /// Processes all the new data into the channel internal buffer and
    /// returns the number of bytes that was read.
    pub fn poll_rtt(&mut self, core: &mut Core) -> Result<Option<usize>, Error> {
        match self.up_channel.read(core, self.rtt_buffer.as_mut())? {
            0 => Ok(None),
            count => Ok(Some(count)),
        }
    }

    /// Retrieves available data from the channel and if available, returns `Some(channel_number:String, formatted_data:String)`.
    /// If no data is available, or we encounter a recoverable error, it returns `None` value for `formatted_data`.
    /// Non-recoverable errors are propagated to the caller.
    pub fn poll_process_rtt_data(
        &mut self,
        core: &mut Core,
        collector: &mut impl ChannelDataCallbacks,
    ) -> Result<(), Error> {
        let Some(bytes_read) = self.poll_rtt(core)? else {
            return Ok(());
        };

        let buffer = &self.rtt_buffer[..bytes_read];

        self.data_format.process(self.number(), buffer, collector)
    }

    /// Clean up temporary changes made to the channel.
    pub fn clean_up(&mut self, core: &mut Core) -> Result<(), Error> {
        if let Some(mode) = self.original_mode.take() {
            self.up_channel.set_mode(core, mode)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct RttActiveDownChannel {
    pub down_channel: DownChannel,
}

impl RttActiveDownChannel {
    pub fn new(down_channel: DownChannel) -> Self {
        Self { down_channel }
    }

    pub fn channel_name(&self) -> String {
        self.down_channel
            .name()
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("Unnamed RTT down channel - {}", self.down_channel.number()))
    }

    pub fn number(&self) -> usize {
        self.down_channel.number()
    }

    pub fn push_rtt(&mut self, core: &mut Core<'_>, data: &str) -> Result<(), Error> {
        self.down_channel.write(core, data.as_bytes()).map(|_| ())
    }
}

/// Once an active connection with the Target RTT control block has been established, we configure
/// each of the active channels, and hold essential state information for successful communication.
#[derive(Debug)]
pub struct RttActiveTarget {
    control_block_addr: u64,
    pub active_up_channels: Vec<RttActiveUpChannel>,
    pub active_down_channels: Vec<RttActiveDownChannel>,
}

/// defmt information common to all defmt channels.
pub struct DefmtState {
    pub table: defmt_decoder::Table,
    pub locs: Option<defmt_decoder::Locations>,
}
impl DefmtState {
    pub fn try_from_bytes(buffer: &[u8]) -> Result<Option<Self>, Error> {
        let Some(table) =
            defmt_decoder::Table::parse(buffer).with_context(|| "Failed to parse defmt data")?
        else {
            return Ok(None);
        };

        let locs = table
            .get_locations(buffer)
            .with_context(|| "Failed to parse defmt data")?;

        let locs = if !table.is_empty() && locs.is_empty() {
            tracing::warn!("Insufficient DWARF info; compile your program with `debug = 2` to enable location info.");
            None
        } else if table.indices().all(|idx| locs.contains_key(&(idx as u64))) {
            Some(locs)
        } else {
            tracing::warn!("Location info is incomplete; it will be omitted from the output.");
            None
        };
        Ok(Some(DefmtState { table, locs }))
    }
}

impl fmt::Debug for DefmtState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DefmtState").finish_non_exhaustive()
    }
}

impl RttActiveTarget {
    /// RttActiveTarget collects references to all the `RttActiveChannel`s, for latter polling/pushing of data.
    pub fn new(
        core: &mut Core,
        rtt: Rtt,
        defmt_state: Option<Arc<DefmtState>>,
        rtt_config: &RttConfig,
        timestamp_offset: UtcOffset,
    ) -> Result<Self, Error> {
        let control_block_addr = rtt.ptr();
        let mut active_up_channels = Vec::with_capacity(rtt.up_channels.len());

        // For each channel configured in the RTT Control Block (`Rtt`), check if there are
        // additional user configuration in a `RttChannelConfig`. If not, apply defaults.
        for channel in rtt.up_channels.into_iter() {
            let channel_config = rtt_config
                .channel_config(channel.number())
                .cloned()
                .unwrap_or_default();
            active_up_channels.push(RttActiveUpChannel::new(
                core,
                channel,
                &channel_config,
                timestamp_offset,
                defmt_state.clone(),
            )?);
        }

        let active_down_channels = rtt
            .down_channels
            .into_iter()
            .map(RttActiveDownChannel::new)
            .collect::<Vec<_>>();

        Ok(Self {
            control_block_addr,
            active_up_channels,
            active_down_channels,
        })
    }

    pub fn get_rtt_symbol<T: Read + Seek>(file: &mut T) -> Option<u64> {
        let mut buffer = Vec::new();
        if file.read_to_end(&mut buffer).is_ok() {
            if let Some(rtt) = Self::get_rtt_symbol_from_bytes(buffer.as_slice()) {
                return Some(rtt);
            }
        }

        tracing::warn!(
            "No RTT header info was present in the ELF file. Does your firmware run RTT?"
        );
        None
    }

    pub fn get_rtt_symbol_from_bytes(buffer: &[u8]) -> Option<u64> {
        if let Ok(binary) = goblin::elf::Elf::parse(buffer) {
            for sym in &binary.syms {
                if binary.strtab.get_at(sym.st_name) == Some("_SEGGER_RTT") {
                    return Some(sym.st_value);
                }
            }
        }

        None
    }

    /// Polls the RTT target on all channels and returns available data.
    /// An error on any channel will return an error instead of incomplete data.
    pub fn poll_rtt_fallible(
        &mut self,
        core: &mut Core,
        collector: &mut impl ChannelDataCallbacks,
    ) -> Result<(), Error> {
        for channel in self.active_up_channels.iter_mut() {
            channel.poll_process_rtt_data(core, collector)?;
        }
        Ok(())
    }

    /// Clean up temporary changes made to the channels.
    pub fn clean_up(&mut self, core: &mut Core) -> Result<(), Error> {
        for channel in self.active_up_channels.iter_mut() {
            channel.clean_up(core)?;
        }
        Ok(())
    }

    /// Overwrites the control block with zeros. This is useful after resets.
    pub fn clear_control_block(&mut self, core: &mut Core) -> Result<(), Error> {
        let zeros = vec![0; Rtt::control_block_size(core)];
        core.write(self.control_block_addr, &zeros)?;
        self.active_down_channels.clear();
        self.active_up_channels.clear();
        Ok(())
    }
}
