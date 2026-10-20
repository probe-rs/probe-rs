use crate::{
    CoreOptions,
    util::{cli, common_options::ProbeOptions},
};
use probe_rs_rpc::reset::{ResetMethod, ResetTargetRequest};
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

    /// How to reset the target. `pin` fails if `reset_line_connected` is false in the probe settings.
    #[clap(long, value_enum)]
    method: Option<Method>,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let attach = cli::attach_request(&client, self.common, false).await?;

        // With `reset_line_connected` in the probe settings, a pin reset resets the whole chip.
        let result = cli::with_slow_attach_feedback(client.reset_target(ResetTargetRequest {
            attach,
            method: self.method.map(|method| match method {
                Method::Pin => ResetMethod::Pin,
                Method::Core => ResetMethod::Core,
            }),
            core: self.shared.core as u32,
        }))
        .await?;

        cli::attach_outcome(&client, result).await
    }
}
