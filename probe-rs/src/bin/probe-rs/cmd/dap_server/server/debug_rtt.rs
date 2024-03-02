use crate::cmd::dap_server::{
    debug_adapter::{dap::adapter::*, protocol::ProtocolAdapter},
    DebuggerError,
};
use crate::util::rtt;
use probe_rs::Core;

/// Manage the active RTT target for a specific SessionData, as well as provide methods to reliably move RTT from target, through the debug_adapter, to the client.
pub struct RttConnection {
    /// The connection to RTT on the target
    pub(crate) target_rtt: rtt::RttActiveTarget,
    /// Some status fields and methods to ensure continuity in flow of data from target to debugger to client.
    pub(crate) debugger_rtt_channels: Vec<DebuggerRttChannel>,
}

impl RttConnection {
    /// Polls all the available channels for data and transmits data to the client.
    /// If at least one channel had data, then return a `true` status.
    pub fn process_rtt_data<P: ProtocolAdapter>(
        &mut self,
        debug_adapter: &mut DebugAdapter<P>,
        target_core: &mut Core,
    ) -> bool {
        let mut at_least_one_channel_had_data = false;
        for debugger_rtt_channel in self.debugger_rtt_channels.iter_mut() {
            at_least_one_channel_had_data |=
                debugger_rtt_channel.poll_rtt_data(target_core, debug_adapter, &mut self.target_rtt)
        }
        at_least_one_channel_had_data
    }
}

pub(crate) struct DebuggerRttChannel {
    pub(crate) channel_number: usize,
    // We will not poll target RTT channels until we have confirmation from the client that the output window has been opened.
    pub(crate) has_client_window: bool,
}

impl DebuggerRttChannel {
    /// Poll and retrieve data from the target, and send it to the client, depending on the state of `hasClientWindow`.
    /// Doing this selectively ensures that we don't pull data from target buffers until we have an output window, and also helps us drain buffers after the target has entered a `is_halted` state.
    /// Errors will be reported back to the `debug_adapter`, and the return `bool` value indicates whether there was available data that was processed.
    pub(crate) fn poll_rtt_data<P: ProtocolAdapter>(
        &mut self,
        core: &mut Core,
        debug_adapter: &mut DebugAdapter<P>,
        rtt_target: &mut rtt::RttActiveTarget,
    ) -> bool {
        if !self.has_client_window {
            return false;
        }

        let Some(rtt_channel) = rtt_target
            .active_channels
            .iter_mut()
            .find(|active_channel| active_channel.number() == Some(self.channel_number))
        else {
            return false;
        };

        match rtt_channel.get_rtt_data(core, rtt_target.defmt_state.as_ref()) {
            Ok(Some((channel, data))) => return debug_adapter.rtt_output(channel, data),
            Ok(None) => {}
            Err(rtt_error) => {
                debug_adapter
                    .show_error_message(&DebuggerError::Other(rtt_error))
                    .ok();
            }
        }

        false
    }
}
