use std::net::SocketAddr;

use probe_rs::{Core, rtt::Error};

use crate::{
    cmd::cargo_embed::rttui::tcp::TcpPublisher,
    util::rtt::{
        ProcessedRttData, RttActiveUpChannel, RttDataHandler, RttDecoder, client::RttClient,
    },
};

pub enum ChannelData {
    Strings { messages: Vec<String> },
    Binary { data: Vec<u8> },
}

impl RttDataHandler for (&mut Option<TcpPublisher>, &mut ChannelData) {
    async fn on_string_data(&mut self, data: String) -> Result<(), Error> {
        if let Some(stream) = self.0 {
            stream.send(data.as_bytes());
        }

        let ChannelData::Strings { messages } = &mut self.1 else {
            unreachable!()
        };

        messages.push(data);
        Ok(())
    }

    async fn on_binary_data(&mut self, incoming: &[u8]) -> Result<(), Error> {
        if let Some(stream) = self.0 {
            stream.send(incoming);
        }

        let ChannelData::Binary { data } = &mut self.1 else {
            unreachable!()
        };

        data.extend_from_slice(incoming);
        Ok(())
    }
}

/// Unified channel that handles both up (target -> host) and down (host -> target) directions.
/// Can optionally forward data to/from TCP for bi-directional communication.
/// Supports up-only, down-only, or both directions.
pub struct UpDownChannel {
    // Up channel (target -> host) - optional
    up_channel_number: Option<u32>,
    channel_name: String,
    data_format: Option<RttDecoder>, // Only needed for up channels
    pub data: ChannelData,

    // Down channel (host -> target) - optional
    down_channel_number: Option<u32>,

    // Shared TCP connection for bi-directional communication
    tcp_stream: Option<TcpPublisher>,

    // Buffer for TCP -> RTT data
    tcp_read_buffer: Vec<u8>,
}

impl UpDownChannel {
    /// Create a channel with an up channel (target -> host).
    pub fn new_with_up(
        rtt_channel: &RttActiveUpChannel,
        data_format: RttDecoder,
        tcp_stream: Option<SocketAddr>,
        down_channel_number: Option<u32>,
    ) -> Self {
        Self {
            data: if data_format.is_binary() {
                ChannelData::Binary { data: Vec::new() }
            } else {
                ChannelData::Strings {
                    messages: Vec::new(),
                }
            },
            data_format: Some(data_format),
            tcp_stream: tcp_stream.map(TcpPublisher::new),
            up_channel_number: Some(rtt_channel.number()),
            channel_name: rtt_channel.channel_name().to_string(),
            down_channel_number,
            tcp_read_buffer: Vec::with_capacity(1024),
        }
    }

    /// Create a down-only channel (host -> target).
    pub fn new_down_only(
        rtt_channel: &crate::util::rtt::RttActiveDownChannel,
        tcp_stream: Option<SocketAddr>,
    ) -> Self {
        Self {
            data: ChannelData::Strings {
                messages: Vec::new(),
            },
            data_format: None,
            tcp_stream: tcp_stream.map(TcpPublisher::new),
            up_channel_number: None,
            channel_name: rtt_channel.channel_name(),
            down_channel_number: Some(rtt_channel.number()),
            tcp_read_buffer: Vec::with_capacity(1024),
        }
    }

    /// Poll RTT up channel (target -> host) and forward to TCP if configured.
    pub async fn poll_rtt_up(
        &mut self,
        core: &mut Core<'_>,
        client: &mut RttClient,
    ) -> Result<(), Error> {
        let Some(up_channel) = self.up_channel_number else {
            return Ok(());
        };

        let Some(data_format) = &mut self.data_format else {
            return Ok(());
        };

        let bytes = client.poll_channel(core, up_channel)?;

        match data_format.process(bytes)? {
            Some(ProcessedRttData::Binary(bytes)) => {
                (&mut self.tcp_stream, &mut self.data)
                    .on_binary_data(bytes)
                    .await?;
            }
            Some(ProcessedRttData::String(bytes)) => {
                (&mut self.tcp_stream, &mut self.data)
                    .on_string_data(bytes)
                    .await?;
            }
            None => {}
        }

        Ok(())
    }

    /// Poll TCP for incoming data and forward to RTT down channel (host -> target).
    pub fn poll_tcp_down(
        &mut self,
        core: &mut Core<'_>,
        client: &mut RttClient,
    ) -> Result<(), Error> {
        let Some(down_channel) = self.down_channel_number else {
            return Ok(());
        };

        // Read from TCP if configured
        if let Some(tcp) = self.tcp_stream.as_mut() {
            tracing::info!("read from tcp");
            // Read available data from TCP into buffer
            let mut tcp_buf = [0u8; 1024];
            while let Some(n) = tcp.try_read(&mut tcp_buf) {
                if n > 0 {
                    self.tcp_read_buffer.extend_from_slice(&tcp_buf[..n]);
                }
            }
        }

        // Write buffered data to RTT down channel in chunks
        // Note: Since write_down_channel doesn't return bytes written, we write in small chunks.
        const CHUNK_SIZE: usize = 64;
        let mut attempts = 0;
        const MAX_ATTEMPTS: usize = 10; // Limit attempts per poll to avoid blocking

        while !self.tcp_read_buffer.is_empty() && attempts < MAX_ATTEMPTS {
            attempts += 1;
            let chunk_size = self.tcp_read_buffer.len().min(CHUNK_SIZE);
            let chunk = &self.tcp_read_buffer[..chunk_size];

            // Try to write the chunk. The RTT implementation will write as much as possible.
            client.write_down_channel(core, down_channel, chunk)?;

            // Remove the chunk from buffer. If nothing was written (buffer full),
            // we'll lose this chunk, but continue with the next one.
            self.tcp_read_buffer.drain(..chunk_size);
        }

        Ok(())
    }

    pub(crate) fn channel_name(&self) -> &str {
        &self.channel_name
    }

    pub(crate) fn down_channel_number(&self) -> Option<u32> {
        self.down_channel_number
    }
}
