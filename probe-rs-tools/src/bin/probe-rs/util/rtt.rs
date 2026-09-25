use postcard_schema::Schema;
use probe_rs::rtt::{self, DownChannel, Error, Rtt, UpChannel};
use probe_rs::{Core, MemoryInterface};
use probe_rs_rpc::rtt_config::{ChannelMode, RttChannelConfig};
use serde::{Deserialize, Serialize};

pub(crate) mod client;
pub(crate) mod processing;

pub use processing::*;

pub(crate) fn from_wire_channel_mode(mode: ChannelMode) -> rtt::ChannelMode {
    match mode {
        ChannelMode::NoBlockSkip => rtt::ChannelMode::NoBlockSkip,
        ChannelMode::NoBlockTrim => rtt::ChannelMode::NoBlockTrim,
        ChannelMode::BlockIfFull => rtt::ChannelMode::BlockIfFull,
    }
}

pub(crate) fn to_wire_channel_mode(mode: rtt::ChannelMode) -> ChannelMode {
    match mode {
        rtt::ChannelMode::NoBlockSkip => ChannelMode::NoBlockSkip,
        rtt::ChannelMode::NoBlockTrim => ChannelMode::NoBlockTrim,
        rtt::ChannelMode::BlockIfFull => ChannelMode::BlockIfFull,
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

    /// Default channel configuration.
    #[serde(default)]
    pub default_config: RttChannelConfig,
}

impl RttConfig {
    /// Returns the configuration for the specified channel number, if it exists.
    pub fn channel_config(&self, channel_number: u32) -> &RttChannelConfig {
        self.channels
            .iter()
            .find(|ch| ch.channel_number == Some(channel_number))
            .unwrap_or(&self.default_config)
    }
}

#[derive(Debug)]
pub struct RttActiveUpChannel {
    pub up_channel: UpChannel,

    rtt_buffer: Box<[u8]>,
    bytes_buffered: usize,

    /// If set, the original mode of the channel before we first changed it. Upon exit we should do
    /// our best to restore the original mode.
    original_mode: Option<ChannelMode>,
}

impl RttActiveUpChannel {
    pub fn new(up_channel: UpChannel) -> Self {
        Self {
            rtt_buffer: vec![0; up_channel.buffer_size().max(1)].into_boxed_slice(),
            bytes_buffered: 0,
            up_channel,
            original_mode: None,
        }
    }

    pub fn change_mode(&mut self, core: &mut Core, mode: ChannelMode) -> Result<(), Error> {
        if self.original_mode.is_none() {
            self.original_mode = Some(to_wire_channel_mode(self.up_channel.mode(core)?));
        }
        self.up_channel.set_mode(core, from_wire_channel_mode(mode))
    }

    pub fn channel_name(&self) -> String {
        self.up_channel
            .name()
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("Unnamed RTT up channel - {}", self.up_channel.number()))
    }

    /// Returns the buffer size in bytes. Note that the usable size is one byte less due to how the
    /// ring buffer is implemented.
    pub fn buffer_size(&self) -> usize {
        self.up_channel.buffer_size()
    }

    pub fn number(&self) -> u32 {
        self.up_channel.number() as u32
    }

    /// Reads available channel data into the internal buffer.
    pub fn poll(&mut self, core: &mut Core) -> Result<(), Error> {
        self.bytes_buffered = self.up_channel.read(core, self.rtt_buffer.as_mut())?;
        Ok(())
    }

    /// Returns the buffered data.
    pub fn buffered_data(&self) -> &[u8] {
        &self.rtt_buffer[..self.bytes_buffered]
    }

    /// Clean up temporary changes made to the channel.
    pub fn clean_up(&mut self, core: &mut Core) -> Result<(), Error> {
        if let Some(mode) = self.original_mode.take() {
            self.up_channel
                .set_mode(core, from_wire_channel_mode(mode))?;
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

    /// Returns the buffer size in bytes. Note that the usable size is one byte less due to how the
    /// ring buffer is implemented.
    pub fn buffer_size(&self) -> usize {
        self.down_channel.buffer_size()
    }

    pub fn number(&self) -> u32 {
        self.down_channel.number() as u32
    }

    /// Writes as much of `data` as the target's buffer accepts, returning how many
    /// bytes were taken. This does not block: a caller that discards the count
    /// cannot know what to retry, and silently loses the remainder.
    pub fn write(&mut self, core: &mut Core<'_>, data: impl AsRef<[u8]>) -> Result<usize, Error> {
        self.down_channel.write(core, data.as_ref())
    }
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
    pub fn new(rtt: Rtt) -> Result<Self, Error> {
        let control_block_addr = rtt.ptr();

        let active_up_channels = rtt
            .up_channels
            .into_iter()
            .map(RttActiveUpChannel::new)
            .collect::<Vec<_>>();

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
    pub fn poll_channel(&mut self, core: &mut Core, channel_idx: u32) -> Result<(), Error> {
        let channel_idx = channel_idx as usize;
        if let Some(channel) = self.active_up_channels.get_mut(channel_idx) {
            channel.poll(core)
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

    /// Send data to a down channel, returning how many bytes it accepted.
    pub fn write_down_channel(
        &mut self,
        core: &mut Core,
        channel_idx: u32,
        data: impl AsRef<[u8]>,
    ) -> Result<usize, Error> {
        let channel_idx = channel_idx as usize;
        if let Some(channel) = self.active_down_channels.get_mut(channel_idx) {
            channel.write(core, data)
        } else {
            Err(Error::MissingChannel(channel_idx))
        }
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
        let zeros = vec![0; Rtt::control_block_size()];
        core.write(self.control_block_addr, &zeros)?;
        self.active_down_channels.clear();
        self.active_up_channels.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use probe_rs::rtt;

    #[test]
    fn channel_mode_wire_conversion_round_trips() {
        for mode in [
            rtt::ChannelMode::NoBlockSkip,
            rtt::ChannelMode::NoBlockTrim,
            rtt::ChannelMode::BlockIfFull,
        ] {
            assert_eq!(from_wire_channel_mode(to_wire_channel_mode(mode)), mode);
        }
    }
}
