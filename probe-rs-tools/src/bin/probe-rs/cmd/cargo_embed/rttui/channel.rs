use std::net::SocketAddr;

use probe_rs::{rtt::Error, Core};

use crate::{
    cmd::cargo_embed::rttui::tcp::TcpPublisher,
    util::rtt::{client::RttClient, ChannelDataCallbacks, RttActiveUpChannel},
};

pub enum ChannelData {
    Strings { messages: Vec<String> },
    Binary { data: Vec<u8> },
}

impl ChannelDataCallbacks for (&mut Option<TcpPublisher>, &mut ChannelData) {
    fn on_string_data(&mut self, _channel: usize, data: String) -> Result<(), Error> {
        if let Some(ref mut stream) = self.0 {
            stream.send(data.as_bytes());
        }

        let ChannelData::Strings { messages } = &mut self.1 else {
            unreachable!()
        };

        messages.push(data);
        Ok(())
    }

    fn on_binary_data(&mut self, _channel: usize, incoming: &[u8]) -> Result<(), Error> {
        if let Some(ref mut stream) = self.0 {
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
    channel_number: usize,
    tcp_stream: Option<TcpPublisher>,
    channel_name: String,
    pub data: ChannelData,
}

impl UpChannel {
    pub fn new(rtt_channel: &RttActiveUpChannel, tcp_stream: Option<SocketAddr>) -> Self {
        Self {
            data: if rtt_channel.data_format.is_binary() {
                ChannelData::Binary { data: Vec::new() }
            } else {
                ChannelData::Strings {
                    messages: Vec::new(),
                }
            },
            tcp_stream: tcp_stream.map(TcpPublisher::new),
            channel_number: rtt_channel.number(),
            channel_name: rtt_channel.channel_name().to_string(),
        }
    }

    pub fn poll_rtt(&mut self, core: &mut Core<'_>, client: &mut RttClient) -> Result<(), Error> {
        client.poll_channel(
            core,
            self.channel_number,
            &mut (&mut self.tcp_stream, &mut self.data),
        )
    }

    pub(crate) fn channel_name(&self) -> &str {
        &self.channel_name
    }
}
