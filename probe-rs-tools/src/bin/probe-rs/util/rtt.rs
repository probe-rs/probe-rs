use postcard_schema::Schema;
use probe_rs::rtt::{self, DownChannel, Error, Rtt, UpChannel};
use probe_rs::{Core, MemoryInterface};
use serde::{Deserialize, Serialize};

pub(crate) mod client;
pub(crate) mod processing;

pub use processing::*;

/// Used by serde to provide defaults for `RttChannelConfig::show_timestamps`
fn default_show_timestamps() -> bool {
    true
}

#[derive(
    Debug, Copy, Clone, PartialEq, Eq, Default, docsplay::Display, Serialize, Deserialize, Schema,
)]
pub enum DataFormat {
    #[default]
    /// string
    String,
    /// binary
    BinaryLE,
    /// defmt
    Defmt,
}

/// Specifies what to do when a channel doesn't have enough buffer space for a complete write on the
/// target side.
#[derive(Clone, Copy, Eq, PartialEq, Debug, Serialize, Deserialize, Schema)]
#[repr(u32)]
pub enum ChannelMode {
    /// Skip writing the data completely if it doesn't fit in its entirety.
    NoBlockSkip = 0,

    /// Write as much as possible of the data and ignore the rest.
    NoBlockTrim = 1,

    /// Block (spin) if the buffer is full. Note that if the application writes within a critical
    /// section, using this mode can cause the application to freeze if the buffer becomes full and
    /// is not read by the host.
    BlockIfFull = 2,
}

impl From<ChannelMode> for rtt::ChannelMode {
    fn from(mode: ChannelMode) -> Self {
        match mode {
            ChannelMode::NoBlockSkip => rtt::ChannelMode::NoBlockSkip,
            ChannelMode::NoBlockTrim => rtt::ChannelMode::NoBlockTrim,
            ChannelMode::BlockIfFull => rtt::ChannelMode::BlockIfFull,
        }
    }
}

impl From<rtt::ChannelMode> for ChannelMode {
    fn from(mode: rtt::ChannelMode) -> Self {
        match mode {
            rtt::ChannelMode::NoBlockSkip => ChannelMode::NoBlockSkip,
            rtt::ChannelMode::NoBlockTrim => ChannelMode::NoBlockTrim,
            rtt::ChannelMode::BlockIfFull => ChannelMode::BlockIfFull,
        }
    }
}

/// The initial configuration for RTT (Real Time Transfer). This configuration is complimented with the additional information specified for each of the channels in `RttChannel`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, Schema)]
pub struct RttConfig {
    #[serde(default, rename = "rttEnabled")]
    pub enabled: bool,

    /// Configure data_format and show_timestamps for select channels
    #[serde(default = "Vec::new", rename = "rttChannelFormats")]
    pub channels: Vec<RttChannelConfig>,
}

impl RttConfig {
    /// Returns the configuration for the specified channel number, if it exists.
    pub fn channel_config(&self, channel_number: u32) -> Option<&RttChannelConfig> {
        self.channels
            .iter()
            .find(|ch| ch.channel_number == Some(channel_number))
    }
}

/// The User specified configuration for each active RTT Channel.
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct RttChannelConfig {
    pub channel_number: Option<u32>,
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

#[derive(Debug)]
pub struct RttActiveUpChannel {
    pub up_channel: UpChannel,

    rtt_buffer: Box<[u8]>,
    bytes_buffered: usize,

    /// If set, the original mode of the channel before we first changed it. Upon exit we should do
    /// our best to restore the original mode.
    original_mode: Option<rtt::ChannelMode>,
}

impl RttActiveUpChannel {
    pub async fn new(up_channel: UpChannel) -> Self {
        Self {
            rtt_buffer: vec![0; up_channel.buffer_size().max(1)].into_boxed_slice(),
            bytes_buffered: 0,
            up_channel,
            original_mode: None,
        }
    }

    pub async fn change_mode(
        &mut self,
        core: &mut Core<'_>,
        mode: ChannelMode,
    ) -> Result<(), Error> {
        if self.original_mode.is_none() {
            self.original_mode = Some(self.up_channel.mode(core).await?);
        }
        self.up_channel.set_mode(core, mode.into()).await
    }

    pub fn channel_name(&self) -> String {
        self.up_channel
            .name()
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("Unnamed RTT up channel - {}", self.up_channel.number()))
    }

    pub fn number(&self) -> u32 {
        self.up_channel.number() as u32
    }

    /// Reads available channel data into the internal buffer.
    pub async fn poll(&mut self, core: &mut Core<'_>) -> Result<(), Error> {
        self.bytes_buffered = self.up_channel.read(core, self.rtt_buffer.as_mut()).await?;
        Ok(())
    }

    /// Returns the buffered data.
    pub fn buffered_data(&self) -> &[u8] {
        &self.rtt_buffer[..self.bytes_buffered]
    }

    /// Clean up temporary changes made to the channel.
    pub async fn clean_up(&mut self, core: &mut Core<'_>) -> Result<(), Error> {
        if let Some(mode) = self.original_mode.take() {
            self.up_channel.set_mode(core, mode).await?;
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

    pub fn number(&self) -> u32 {
        self.down_channel.number() as u32
    }

    pub async fn write(
        &mut self,
        core: &mut Core<'_>,
        data: impl AsRef<[u8]>,
    ) -> Result<(), Error> {
        self.down_channel
            .write(core, data.as_ref())
            .await
            .map(|_| ())
    }
}

/// Error type for RTT symbol lookup.
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum RttSymbolError {
    /// RTT symbol not found in the ELF file.
    RttSymbolNotFound,
    /// Failed to parse the firmware as an ELF file.
    Goblin(#[source] goblin::error::Error),
}

/// Once an active connection with the Target RTT control block has been established, we configure
/// each of the active channels, and hold essential state information for successful communication.
#[derive(Debug)]
pub struct RttConnection {
    control_block_addr: u64,
    pub active_up_channels: Vec<RttActiveUpChannel>,
    pub active_down_channels: Vec<RttActiveDownChannel>,
}

impl RttConnection {
    /// RttActiveTarget collects references to all the `RttActiveChannel`s, for latter polling/pushing of data.
    pub async fn new(rtt: Rtt) -> Result<Self, Error> {
        let control_block_addr = rtt.ptr();

        let mut active_up_channels = vec![];
        for channel in rtt.up_channels.into_iter() {
            active_up_channels.push(RttActiveUpChannel::new(channel).await);
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

    /// Polls the RTT target on all channels and returns available data.
    /// An error on any channel will return an error instead of incomplete data.
    pub async fn poll_channel(
        &mut self,
        core: &mut Core<'_>,
        channel_idx: u32,
    ) -> Result<(), Error> {
        let channel_idx = channel_idx as usize;
        if let Some(channel) = self.active_up_channels.get_mut(channel_idx) {
            channel.poll(core).await
        } else {
            Err(Error::MissingChannel(channel_idx))
        }
    }

    pub fn channel_data(&self, channel_idx: u32) -> Result<&[u8], Error> {
        let channel_idx = channel_idx as usize;
        if let Some(channel) = self.active_up_channels.get(channel_idx) {
            Ok(channel.buffered_data())
        } else {
            Err(Error::MissingChannel(channel_idx))
        }
    }

    /// Send data to a down channel.
    pub async fn write_down_channel(
        &mut self,
        core: &mut Core<'_>,
        channel_idx: u32,
        data: impl AsRef<[u8]>,
    ) -> Result<(), Error> {
        let channel_idx = channel_idx as usize;
        if let Some(channel) = self.active_down_channels.get_mut(channel_idx) {
            channel.write(core, data).await
        } else {
            Err(Error::MissingChannel(channel_idx))
        }
    }

    /// Clean up temporary changes made to the channels.
    pub async fn clean_up(&mut self, core: &mut Core<'_>) -> Result<(), Error> {
        for channel in self.active_up_channels.iter_mut() {
            channel.clean_up(core).await?;
        }
        Ok(())
    }

    /// Overwrites the control block with zeros. This is useful after resets.
    pub async fn clear_control_block(&mut self, core: &mut Core<'_>) -> Result<(), Error> {
        let zeros = vec![0; Rtt::control_block_size(core)];
        core.write(self.control_block_addr, &zeros).await?;
        self.active_down_channels.clear();
        self.active_up_channels.clear();
        Ok(())
    }
}

pub fn get_rtt_symbol_from_bytes(buffer: &[u8]) -> Result<u64, RttSymbolError> {
    match goblin::elf::Elf::parse(buffer) {
        Ok(binary) => {
            for sym in &binary.syms {
                if binary.strtab.get_at(sym.st_name) == Some("_SEGGER_RTT") {
                    return Ok(sym.st_value);
                }
            }
            Err(RttSymbolError::RttSymbolNotFound)
        }
        Err(err) => Err(RttSymbolError::Goblin(err)),
    }
}
