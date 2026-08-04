use crate::{
    cmd::dap_server::{DebuggerError, debug_adapter::dap::adapter::*},
    rpc::{Key, RttClient},
    util::rtt::RttDecoder,
};
use anyhow::anyhow;
use probe_rs::rtt::Error as RttError;
use probe_rs_rpc_client::SessionInterface;

/// Per-channel result of a batched [`RemoteRttClient::poll_channels_remote`]
/// call.
pub(crate) type ChannelPollResults = Vec<(u32, Result<Vec<u8>, RttError>)>;

/// Handle to the server-side [`RttClient`].
pub struct RemoteRttClient {
    session: SessionInterface,
    rtt_client: Key<RttClient>,
}

impl RemoteRttClient {
    pub(crate) fn new(session: SessionInterface, rtt_client: Key<RttClient>) -> Self {
        Self {
            session,
            rtt_client,
        }
    }

    /// Poll the server-owned RTT client through `rtt/poll_up`.
    async fn poll_channels_remote(
        &mut self,
        channels: &[u32],
    ) -> Result<ChannelPollResults, RttError> {
        let results = self
            .session
            .poll_rtt_up(self.rtt_client, channels.to_vec())
            .await
            .map_err(|e| RttError::Other(e.into()))?;
        Ok(results
            .into_iter()
            .map(|r| {
                let res = match r.result {
                    Ok(data) => Ok(data),
                    Err(e) => Err(RttError::Other(anyhow!(e))),
                };
                (r.channel, res)
            })
            .collect())
    }

    /// Clean up the RTT connection server-side.
    async fn clean_up_async(&mut self) -> Result<(), RttError> {
        self.session
            .clean_up_rtt(self.rtt_client)
            .await
            .map_err(|e| RttError::Other(e.into()))
    }

    /// Write data to a down channel.
    ///
    /// Single attempt, and the accepted count is discarded: this signature
    /// cannot report a short write, so a full target buffer still loses the
    /// tail here. Kept as it was rather than fixed blind -- see the callers.
    pub(crate) async fn write_down_async(
        &mut self,
        channel: u32,
        data: Vec<u8>,
    ) -> Result<(), RttError> {
        self.session
            .send_to_rtt(self.rtt_client, channel, data, 0)
            .await
            .map(|_| ())
            .map_err(|e| RttError::Other(e.into()))
    }
}

/// Manage the active RTT target for a specific SessionData, as well as provide methods to reliably move RTT from target, through the debug_adapter, to the client.
pub struct RttConnection {
    /// The connection to RTT on the target
    pub(crate) client: RemoteRttClient,
    /// Some status fields and methods to ensure continuity in flow of data from target to debugger to client.
    pub(crate) debugger_rtt_channels: Vec<DebuggerRttChannel>,
}

impl RttConnection {
    /// Poll all available channels through RPC and transmit data to the DAP
    /// client. Returns `true` if at least one channel had data.
    pub async fn process_rtt_data_remote(&mut self, debug_adapter: &mut DebugAdapter) -> bool {
        // Only poll channels with an open client window; draining a closed
        // channel would drop target buffers prematurely.
        let windowed: Vec<u32> = self
            .debugger_rtt_channels
            .iter()
            .filter(|c| c.has_client_window)
            .map(|c| c.channel_number)
            .collect();
        if windowed.is_empty() {
            return false;
        }

        let results = match self.client.poll_channels_remote(&windowed).await {
            Ok(results) => results,
            Err(error) => {
                debug_adapter
                    .show_error_message(&DebuggerError::Other(anyhow!(error)))
                    .ok();
                return false;
            }
        };

        let mut at_least_one_channel_had_data = false;
        for (channel, result) in results {
            let Some(debugger_rtt_channel) = self
                .debugger_rtt_channels
                .iter_mut()
                .find(|c| c.channel_number == channel)
            else {
                continue;
            };

            let bytes = match result {
                Ok(bytes) => bytes,
                Err(error) => {
                    debug_adapter
                        .show_error_message(&DebuggerError::Other(anyhow!(error)))
                        .ok();
                    continue;
                }
            };

            at_least_one_channel_had_data |=
                debugger_rtt_channel.process_bytes(debug_adapter, &bytes);
        }
        at_least_one_channel_had_data
    }

    /// Clean up the RTT connection, restoring the state changes that we made.
    pub async fn clean_up_async(&mut self) -> Result<(), DebuggerError> {
        self.client
            .clean_up_async()
            .await
            .map_err(|err| DebuggerError::Other(anyhow!(err)))?;
        Ok(())
    }
}

pub(crate) struct DebuggerRttChannel {
    pub(crate) channel_number: u32,
    // We will not poll target RTT channels until we have confirmation from the client that the output window has been opened.
    pub(crate) has_client_window: bool,
    pub(crate) channel_data_format: RttDecoder,
}

impl DebuggerRttChannel {
    /// Decode already-fetched `bytes` for this channel and forward them.
    /// Returns whether any data was emitted.
    pub(crate) fn process_bytes(&mut self, debug_adapter: &mut DebugAdapter, bytes: &[u8]) -> bool {
        match self.channel_data_format.process(bytes).ok().flatten() {
            Some(data) => debug_adapter.rtt_output(self.channel_number, data.to_string()),
            _ => false,
        }
    }
}
