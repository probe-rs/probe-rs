use std::net::SocketAddr;

use crate::{
    cmd::cargo_embed::rttui::tcp::TcpPublisher,
    util::rtt::{ChannelDataCallbacks, ChannelDataConfig, DefmtState, RttActiveUpChannel},
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
        for line in data.split_terminator('\n') {
            messages.push(line.to_string());
        }

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

pub struct UpChannel<'defmt> {
    rtt_channel: RttActiveUpChannel,
    defmt_state: Option<&'defmt DefmtState>,
    tcp_stream: Option<TcpPublisher>,
    pub data: ChannelData,
}

impl<'defmt> UpChannel<'defmt> {
    pub fn new(
        rtt_channel: RttActiveUpChannel,
        defmt_state: Option<&'defmt DefmtState>,
        tcp_stream: Option<SocketAddr>,
    ) -> Self {
        Self {
            data: match rtt_channel.data_format {
                ChannelDataConfig::String { .. } | ChannelDataConfig::Defmt { .. } => {
                    ChannelData::Strings {
                        messages: Vec::new(),
                    }
                }
                ChannelDataConfig::BinaryLE => ChannelData::Binary { data: Vec::new() },
            },
            defmt_state,
            tcp_stream: tcp_stream.map(TcpPublisher::new),
            rtt_channel,
        }
    }

    pub fn poll_rtt(&mut self, core: &mut probe_rs::Core<'_>) -> anyhow::Result<()> {
        self.rtt_channel.poll_process_rtt_data(
            core,
            self.defmt_state,
            &mut (&mut self.tcp_stream, &mut self.data),
        )
    }
}
