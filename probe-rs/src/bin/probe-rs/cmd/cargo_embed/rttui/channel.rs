use std::fmt;
use std::net::SocketAddr;

use probe_rs::rtt::ChannelMode;
use probe_rs::Core;

use crate::cmd::cargo_embed::rttui::tcp::TcpPublisher;
use crate::util::rtt::{
    ChannelDataCallbacks, DataFormat, DefmtState, RttActiveDownChannel, RttActiveUpChannel,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChannelConfig {
    pub up: Option<usize>,
    pub down: Option<usize>,
    pub name: Option<String>,
    pub up_mode: Option<ChannelMode>,
    pub format: DataFormat,
    pub socket: Option<SocketAddr>,
}

pub enum ChannelData {
    String { data: Vec<String> },
    Binary { data: Vec<u8> },
    Defmt { messages: Vec<String> },
}

impl std::fmt::Debug for ChannelData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String { data } => f.debug_struct("String").field("data", data).finish(),
            Self::Binary { data } => f.debug_struct("Binary").field("data", data).finish(),
            Self::Defmt { messages, .. } => f
                .debug_struct("Defmt")
                .field("messages", messages)
                .finish_non_exhaustive(),
        }
    }
}

impl ChannelData {
    pub fn new_string() -> Self {
        Self::String { data: Vec::new() }
    }

    pub fn new_defmt() -> Self {
        Self::Defmt {
            messages: Vec::new(),
        }
    }

    pub fn new_binary() -> Self {
        Self::Binary { data: Vec::new() }
    }

    fn clear(&mut self) {
        match self {
            Self::String { data } => data.clear(),
            Self::Binary { data, .. } => data.clear(),
            Self::Defmt { messages, .. } => messages.clear(),
        }
    }
}

#[derive(Debug)]
pub struct ChannelState<'defmt> {
    up_channel: Option<RttActiveUpChannel>,
    down_channel: Option<RttActiveDownChannel>,
    name: String,
    data: ChannelData,
    defmt_info: Option<&'defmt DefmtState>,
    scroll_offset: usize,
    tcp_socket: Option<TcpPublisher>,
}

impl<'defmt> ChannelState<'defmt> {
    pub fn new(
        up_channel: Option<RttActiveUpChannel>,
        down_channel: Option<RttActiveDownChannel>,
        name: Option<String>,
        data: ChannelData,
        tcp_socket: Option<SocketAddr>,
        defmt_info: Option<&'defmt DefmtState>,
    ) -> Self {
        let name = name
            .or_else(|| up_channel.as_ref().map(|up| up.channel_name.clone()))
            .or_else(|| down_channel.as_ref().map(|down| down.channel_name.clone()))
            .unwrap_or_else(|| "Unnamed channel".to_owned());

        let tcp_socket = tcp_socket.map(TcpPublisher::new);

        Self {
            up_channel,
            down_channel,
            name,
            scroll_offset: 0,
            data,
            tcp_socket,
            defmt_info,
        }
    }

    pub fn has_down_channel(&self) -> bool {
        self.down_channel.is_some()
    }

    pub fn append_char(&mut self, c: char) {
        if let Some(down) = self.down_channel.as_mut() {
            down.input_mut().push(c);
        }
    }

    pub fn pop_char(&mut self) {
        if let Some(down) = self.down_channel.as_mut() {
            down.input_mut().pop();
        }
    }

    pub fn input(&self) -> &str {
        self.down_channel
            .as_ref()
            .map(|down| down.input())
            .unwrap_or_default()
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset += 1;
    }

    pub fn scroll_down(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset -= 1;
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_scroll_offset(&mut self, value: usize) {
        self.scroll_offset = value;
    }

    pub fn clear(&mut self) {
        self.scroll_offset = 0;
        self.data.clear();
        if let Some(up) = self.up_channel.as_mut() {
            up.data_format.clear();
        }
    }

    /// Polls the RTT target for new data on the specified channel.
    ///
    /// Processes all the new data and adds it to the linebuffer of the respective channel.
    ///
    /// # Errors
    /// This function can return a [`time::Error`] if getting the local time or formatting a timestamp fails.
    pub fn poll_rtt(&mut self, core: &mut Core) -> Result<(), time::Error> {
        struct DataCollector<'a> {
            data: &'a mut ChannelData,
            scroll_offset: &'a mut usize,
            tcp_stream: Option<&'a mut TcpPublisher>,
        }
        impl ChannelDataCallbacks for DataCollector<'_> {
            fn on_string_data(&mut self, _channel: usize, data: String) -> anyhow::Result<()> {
                if let Some(ref mut stream) = self.tcp_stream {
                    stream.send(data.as_bytes());
                }

                let messages = match &mut self.data {
                    ChannelData::String { data: messages, .. }
                    | ChannelData::Defmt { messages, .. } => messages,
                    ChannelData::Binary { .. } => {
                        unreachable!()
                    }
                };

                for line in data.split_terminator('\n') {
                    messages.push(line.to_string());
                    if *self.scroll_offset != 0 {
                        *self.scroll_offset += 1;
                    }
                }

                Ok(())
            }

            fn on_binary_data(&mut self, _channel: usize, incoming: &[u8]) -> anyhow::Result<()> {
                if let Some(ref mut stream) = self.tcp_stream {
                    stream.send(incoming);
                }

                match &mut self.data {
                    ChannelData::Binary { data } => data.extend_from_slice(incoming),
                    ChannelData::String { .. } | ChannelData::Defmt { .. } => {
                        unreachable!()
                    }
                }

                Ok(())
            }
        }

        let mut collector = DataCollector {
            data: &mut self.data,
            scroll_offset: &mut self.scroll_offset,
            tcp_stream: self.tcp_socket.as_mut(),
        };

        // TODO: Proper error handling.
        if let Some(channel) = self.up_channel.as_mut() {
            if let Err(err) = channel.poll_process_rtt_data(core, self.defmt_info, &mut collector) {
                tracing::error!("\nError reading from RTT: {}", err);
            }
        }

        Ok(())
    }

    pub fn push_rtt(&mut self, core: &mut Core) {
        if let Some(down_channel) = self.down_channel.as_mut() {
            down_channel.push_rtt(core).unwrap();
        }
    }

    pub(crate) fn data(&self) -> &ChannelData {
        &self.data
    }
}
