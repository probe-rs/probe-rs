use std::net::SocketAddr;

use crate::{
    cmd::cargo_embed::rttui::tcp::TcpPublisher,
    util::rtt::{ChannelDataCallbacks, DefmtState, RttActiveUpChannel},
};

pub enum ChannelData {
    Strings { messages: Vec<String> },
    Binary { data: Vec<u8> },
}

impl ChannelDataCallbacks for (&mut Option<TcpPublisher>, &mut ChannelData) {
    fn on_string_data(&mut self, _channel: usize, data: String) -> anyhow::Result<()> {
        if let Some(ref mut stream) = self.0 {
            stream.send(data.as_bytes());
        }

        let ChannelData::Strings { messages } = &mut self.1 else {
            unreachable!()
        };

        messages.push(data);
        Ok(())
    }

    fn on_binary_data(&mut self, _channel: usize, incoming: &[u8]) -> anyhow::Result<()> {
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
    rtt_channel: RttActiveUpChannel,
    tcp_stream: Option<TcpPublisher>,
    pub data: ChannelData,
}

impl UpChannel {
    pub fn new(rtt_channel: RttActiveUpChannel, tcp_stream: Option<SocketAddr>) -> Self {
        Self {
            data: if rtt_channel.data_format.is_binary() {
                ChannelData::Binary { data: Vec::new() }
            } else {
                ChannelData::Strings {
                    messages: Vec::new(),
                }
            },
            tcp_stream: tcp_stream.map(TcpPublisher::new),
            rtt_channel,
        }
    }

    pub fn poll_rtt(
        &mut self,
        core: &mut probe_rs::Core<'_>,
        defmt_state: Option<&DefmtState>,
    ) -> anyhow::Result<()> {
        self.rtt_channel.poll_process_rtt_data(
            core,
            defmt_state,
            &mut (&mut self.tcp_stream, &mut self.data),
        )
    }

    pub(crate) fn clean_up(&mut self, core: &mut probe_rs::Core<'_>) -> anyhow::Result<()> {
        self.rtt_channel.clean_up(core)
    }
}
