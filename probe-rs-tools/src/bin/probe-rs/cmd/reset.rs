use crate::rpc::functions::probe::convert::to_wire_protocol;
use crate::{
    CoreOptions,
    util::{cli, common_options::ProbeOptions},
};
use probe_rs_rpc::reset::{ResetMethod, ResetProbeRequest};
use probe_rs_rpc_client::RpcClient;

#[derive(clap::ValueEnum, Clone, Copy)]
enum Method {
    /// Pulse the reset line of the probe.
    Pin,
    /// Reset the core through the debug interface.
    Core,
}

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    common: ProbeOptions,

    /// How to reset the target. Overrides `reset_line_connected` of the probe settings.
    #[clap(long, value_enum)]
    method: Option<Method>,
}

impl Cmd {
    pub async fn run(mut self, client: RpcClient) -> anyhow::Result<()> {
        let probe = cli::select_attach_probe(&client, &self.common).await?;
        cli::load_chip_description(&client, &mut self.common).await?;

        // With `reset_line_connected` in the probe settings, a pin reset resets the whole chip.
        let reset = client
            .reset_probe(ResetProbeRequest {
                probe: probe.clone(),
                method: self.method.map(|method| match method {
                    Method::Pin => ResetMethod::Pin,
                    Method::Core => ResetMethod::Core,
                }),
                chip: self.common.chip.clone(),
                protocol: self.common.protocol.map(to_wire_protocol),
                wait_for_probe: self.common.attach_timeout,
            })
            .await?;
        if reset {
            return Ok(());
        }

        let session = cli::attach_selected_probe(&client, probe, self.common, None, false).await?;
        let core = session.core(self.shared.core);

        core.reset().await?;

        Ok(())
    }
}
