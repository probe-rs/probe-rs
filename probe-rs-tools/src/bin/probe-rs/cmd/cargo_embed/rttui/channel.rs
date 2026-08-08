use std::net::SocketAddr;

use probe_rs::rtt::Error;

use crate::{
    cmd::cargo_embed::rttui::tcp::TcpPublisher,
    util::rtt::{ProcessedRttData, RttDataHandler, RttDecoder},
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

pub struct UpChannel {
    channel_number: u32,
    tcp_stream: Option<TcpPublisher>,
    channel_name: String,
    data_format: RttDecoder,
    pub data: ChannelData,
}

impl UpChannel {
    pub fn new(
        channel_number: u32,
        channel_name: String,
        data_format: RttDecoder,
        tcp_stream: Option<SocketAddr>,
    ) -> Self {
        Self {
            data: if data_format.is_binary() {
                ChannelData::Binary { data: Vec::new() }
            } else {
                ChannelData::Strings {
                    messages: Vec::new(),
                }
            },
            data_format,
            tcp_stream: tcp_stream.map(TcpPublisher::new),
            channel_number,
            channel_name,
        }
    }

    pub fn number(&self) -> u32 {
        self.channel_number
    }

    pub async fn push_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        match self.data_format.process(bytes)? {
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

    pub(crate) fn channel_name(&self) -> &str {
        &self.channel_name
    }
}
