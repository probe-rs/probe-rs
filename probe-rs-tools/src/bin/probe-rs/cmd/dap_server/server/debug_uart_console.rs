use crate::util::rtt::{RttDataHandler, RttDecoder};
use crate::{
    cmd::dap_server::{
        DebuggerError,
        debug_adapter::{dap::adapter::DebugAdapter, protocol::ProtocolAdapter},
    },
    util::sifli_uart_client::SifliUartClient,
};
use anyhow::anyhow;
use probe_rs::{probe::sifliuart::console::SifliUartConsole, rtt};
use time::UtcOffset;

pub(crate) const UART_CONSOLE_CHANNEL_NUMBER: u32 = 2048;

pub(crate) struct Connection {
    pub(crate) client: SifliUartClient,
    pub(crate) has_client_window: bool,
    channel_data_format: RttDecoder,
}

impl Connection {
    pub(crate) fn new(console: SifliUartConsole, timestamp_offset: UtcOffset) -> Self {
        Self {
            client: SifliUartClient::new(console),
            has_client_window: false,
            channel_data_format: RttDecoder::String {
                timestamp_offset: Some(timestamp_offset),
                last_line_done: false,
                show_timestamps: false,
            },
        }
    }

    pub(crate) async fn process_output<P: ProtocolAdapter>(
        &mut self,
        debug_adapter: &mut DebugAdapter<P>,
    ) -> bool {
        if !self.has_client_window {
            return false;
        }

        let bytes = match self.client.poll() {
            Ok(bytes) => bytes,
            Err(error) => {
                debug_adapter
                    .show_error_message(&DebuggerError::Other(anyhow!(error)))
                    .ok();
                return false;
            }
        };

        if bytes.is_empty() {
            return false;
        }

        let mut out = StringCollector::default();
        self.channel_data_format.process(bytes, &mut out).await.ok();

        if out.data.is_empty() {
            return false;
        }

        debug_adapter.rtt_output(UART_CONSOLE_CHANNEL_NUMBER, out.data)
    }
}

#[derive(Default)]
struct StringCollector {
    data: String,
}

impl RttDataHandler for StringCollector {
    async fn on_string_data(&mut self, data: String) -> Result<(), rtt::Error> {
        self.data.push_str(&data);
        Ok(())
    }
}
