use anyhow::{anyhow, Result};
use defmt_decoder::log::format::{Formatter, FormatterConfig, FormatterFormat};
use defmt_decoder::DecodeError;
pub use probe_rs::rtt::ChannelMode;
use probe_rs::rtt::{DownChannel, Error, Rtt, ScanRegion, UpChannel};
use probe_rs::{Core, Session};
use probe_rs_target::MemoryRegion;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use std::{
    fmt,
    fmt::Write,
    io::{Read, Seek},
    path::Path,
    thread,
};
use time::{macros::format_description, OffsetDateTime, UtcOffset};

/// Infer the target core from the RTT symbol. Useful for multi-core targets.
pub fn get_target_core_id(session: &mut Session, elf_file: impl AsRef<Path>) -> usize {
    let maybe_core_id = || {
        let mut file = File::open(elf_file).ok()?;
        let address = RttActiveTarget::get_rtt_symbol(&mut file)?;

        tracing::debug!("RTT symbol found at 0x{:08x}", address);

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

        tracing::debug!("RTT symbol is in core {}", core_id);

        Some(core_id)
    };
    maybe_core_id().unwrap_or(0)
}

/// Try to find the RTT control block in the ELF file and attach to it.
///
/// This function can return `Ok(None)` to indicate that RTT is not available on the target.
fn attach_to_rtt(
    core: &mut Core,
    memory_map: &[MemoryRegion],
    rtt_region: &ScanRegion,
) -> Result<Option<Rtt>> {
    tracing::debug!("Initializing RTT");

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

impl std::fmt::Debug for ChannelDataFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
        defmt_state: Option<&DefmtState>,
        collector: &mut impl ChannelDataCallbacks,
    ) -> Result<()> {
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
            } => {
                let string = Self::process_defmt(buffer, defmt_state, formatter, cwd)?;
                collector.on_string_data(number, string)
            }
        }
    }

    fn process_string(
        buffer: &[u8],
        offset: Option<UtcOffset>,
        last_line_done: &mut bool,
    ) -> Result<String> {
        let incoming = String::from_utf8_lossy(buffer);

        let Some(offset) = offset else {
            return Ok(incoming.to_string());
        };

        let timestamp = OffsetDateTime::now_utc()
            .to_offset(offset)
            .format(format_description!(
                "[hour repr:24]:[minute]:[second].[subsecond digits:3]"
            ))?;

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
    ) -> Result<String> {
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
    pub data_format: ChannelDataFormat,
    rtt_buffer: RttBuffer,

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
        defmt_state: Option<&DefmtState>,
    ) -> Result<Self> {
        let is_defmt_channel = up_channel.name() == Some("defmt");

        let data_format = match channel_config.data_format {
            DataFormat::String if !is_defmt_channel => ChannelDataFormat::String {
                timestamp_offset: channel_config.show_timestamps.then_some(timestamp_offset),
                last_line_done: true,
            },

            DataFormat::BinaryLE if !is_defmt_channel => ChannelDataFormat::BinaryLE,

            // either DataFormat::Defmt is configured, or defmt_enabled is true
            _ => {
                let has_timestamp = if let Some(defmt) = defmt_state {
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
                }
            }
        };

        let channel_name = up_channel
            .name()
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                format!(
                    "Unnamed {} RTT up channel - {}",
                    channel_config.data_format,
                    up_channel.number()
                )
            });

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
            rtt_buffer: RttBuffer::new(up_channel.buffer_size()),
            up_channel,
            channel_name,
            data_format,
            original_mode,
        })
    }

    pub fn number(&self) -> usize {
        self.up_channel.number()
    }

    /// Polls the RTT target for new data on the channel represented by `self`.
    /// Processes all the new data into the channel internal buffer and returns the number of bytes that was read.
    pub fn poll_rtt(&mut self, core: &mut Core) -> Option<usize> {
        // Retry loop, in case the probe is temporarily unavailable, e.g. user pressed the `reset` button.
        const RETRY_COUNT: usize = 10;
        for loop_count in 1..=RETRY_COUNT {
            match self.up_channel.read(core, self.rtt_buffer.0.as_mut()) {
                Ok(0) => return None,
                Ok(count) => return Some(count),
                Err(err) if loop_count == RETRY_COUNT => {
                    tracing::error!("\nError reading from RTT: {}", err);
                    return None;
                }
                _ => std::thread::sleep(std::time::Duration::from_millis(50)),
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

        self.data_format
            .process(self.number(), buffer, defmt_state, collector)
    }

    /// Clean up temporary changes made to the channel.
    pub fn clean_up(&mut self, core: &mut Core) -> Result<()> {
        if let Some(mode) = self.original_mode.take() {
            self.up_channel.set_mode(core, mode)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct RttActiveDownChannel {
    pub down_channel: DownChannel,
    pub channel_name: String,
}

impl RttActiveDownChannel {
    pub fn new(down_channel: DownChannel) -> Self {
        let channel_name = down_channel
            .name()
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("Unnamed RTT down channel - {}", down_channel.number()));

        Self {
            down_channel,
            channel_name,
        }
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
    pub active_up_channels: HashMap<usize, RttActiveUpChannel>,
    pub active_down_channels: HashMap<usize, RttActiveDownChannel>,
    pub defmt_state: Option<DefmtState>,
}

/// defmt information common to all defmt channels.
pub struct DefmtState {
    pub table: defmt_decoder::Table,
    pub locs: Option<defmt_decoder::Locations>,
}
impl DefmtState {
    pub fn try_from_bytes(buffer: &[u8]) -> Result<Option<Self>> {
        if let Some(table) = defmt_decoder::Table::parse(buffer)? {
            let locs = {
                let locs = table.get_locations(buffer)?;

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
            Ok(Some(DefmtState { table, locs }))
        } else {
            Ok(None)
        }
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
        rtt: probe_rs::rtt::Rtt,
        defmt_state: Option<DefmtState>,
        rtt_config: &RttConfig,
        timestamp_offset: UtcOffset,
    ) -> Result<Self> {
        let mut active_up_channels = HashMap::new();
        let mut active_down_channels = HashMap::new();

        // For each channel configured in the RTT Control Block (`Rtt`), check if there are additional user configuration in a `RttChannelConfig`. If not, apply defaults.
        for channel in rtt.up_channels.into_iter() {
            let channel_config = rtt_config
                .channel_config(channel.number())
                .cloned()
                .unwrap_or_default();
            active_up_channels.insert(
                channel.number(),
                RttActiveUpChannel::new(
                    core,
                    channel,
                    &channel_config,
                    timestamp_offset,
                    defmt_state.as_ref(),
                )?,
            );
        }

        for channel in rtt.down_channels.into_iter() {
            active_down_channels.insert(channel.number(), RttActiveDownChannel::new(channel));
        }

        // It doesn't make sense to pretend RTT is active, if there are no active up channels
        if active_up_channels.is_empty() {
            return Err(anyhow!(
                "RTT Initialized correctly, but there were no active channels configured"
            ));
        }

        Ok(Self {
            active_up_channels,
            active_down_channels,
            defmt_state,
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
    ) -> Result<()> {
        let defmt_state = self.defmt_state.as_ref();
        for channel in self.active_up_channels.values_mut() {
            channel.poll_process_rtt_data(core, defmt_state, collector)?;
        }
        Ok(())
    }

    /// Clean up temporary changes made to the channels.
    pub fn clean_up(&mut self, core: &mut Core) -> Result<()> {
        for channel in self.active_up_channels.values_mut() {
            channel.clean_up(core)?;
        }
        Ok(())
    }
}

pub(crate) struct RttBuffer(pub Vec<u8>);
impl RttBuffer {
    /// Initialize the buffer and ensure it has enough capacity to match the size of the RTT channel
    /// on the target at the time of instantiation. Doing this now prevents later performance impact
    /// if the buffer capacity has to be grown dynamically.
    pub fn new(buffer_size: usize) -> RttBuffer {
        RttBuffer(vec![0u8; buffer_size.max(1)])
    }
}
impl fmt::Debug for RttBuffer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}

fn try_attach_to_rtt_inner(
    mut attach: impl FnMut(&[MemoryRegion], &ScanRegion) -> Result<Option<Rtt>>,
    timeout: Duration,
    memory_map: &[MemoryRegion],
    rtt_region: &ScanRegion,
) -> Result<Option<Rtt>> {
    let t = Instant::now();
    let mut rtt_init_attempt = 1;
    let mut last_error = None;
    while t.elapsed() < timeout {
        tracing::debug!("Initializing RTT (attempt {})...", rtt_init_attempt);
        rtt_init_attempt += 1;

        match attach(memory_map, rtt_region) {
            Ok(rtt) => return Ok(rtt),
            Err(e) => last_error = Some(e),
        }

        tracing::debug!("Failed to initialize RTT. Retrying until timeout.");
        thread::sleep(Duration::from_millis(10));
    }

    // Timeout
    if let Some(err) = last_error {
        Err(err)
    } else {
        Err(anyhow!("Error setting up RTT"))
    }
}

// TODO: these should be part of the library, but that requires refactoring the errors to not use
// anyhow. Also, we probably should only have one variant of this function and pass the closure
// in as an argument.

/// Try to attach to RTT, with the given timeout.
pub fn try_attach_to_rtt(
    core: &mut Core<'_>,
    timeout: Duration,
    memory_map: &[MemoryRegion],
    rtt_region: &ScanRegion,
) -> Result<Option<Rtt>> {
    try_attach_to_rtt_inner(
        |memory_map, rtt_region| attach_to_rtt(core, memory_map, rtt_region),
        timeout,
        memory_map,
        rtt_region,
    )
}

/// Try to attach to RTT, with the given timeout.
pub fn try_attach_to_rtt_shared(
    session: &parking_lot::FairMutex<Session>,
    core_id: usize,
    memory_map: &[MemoryRegion],
    timeout: Duration,
    rtt_region: &ScanRegion,
) -> Result<Option<Rtt>> {
    try_attach_to_rtt_inner(
        |memory_map, rtt_region| {
            let mut session_handle = session.lock();
            let mut core = session_handle.core(core_id)?;
            attach_to_rtt(&mut core, memory_map, rtt_region)
        },
        timeout,
        memory_map,
        rtt_region,
    )
}
