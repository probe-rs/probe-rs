use crate::rpc::functions::probe::convert::to_wire_protocol;
use crate::{
    CoreOptions,
    util::{cli, common_options::ProbeOptions},
};
use probe_rs_rpc::reset::ResetProbeRequest;
use probe_rs_rpc_client::RpcClient;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    common: ProbeOptions,
}

impl Cmd {
    pub async fn run(mut self, client: RpcClient) -> anyhow::Result<()> {
        let probe = cli::select_attach_probe(&client, &self.common).await?;
        cli::load_chip_description(&client, &mut self.common).await?;

        // With `reset_line_connected` in the probe settings, a pin reset resets the whole chip.
        let reset = client
            .reset_probe(ResetProbeRequest {
                probe: probe.clone(),
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
