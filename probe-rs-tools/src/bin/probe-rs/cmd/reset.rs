use crate::rpc::functions::probe::convert::to_wire_debug_probe_selector;
use crate::{
    CoreOptions,
    util::{cli, common_options::ProbeOptions},
};
use probe_rs_rpc::reset::{ResetHardwareRequest, ResetMethod};
use probe_rs_rpc_client::RpcClient;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    common: ProbeOptions,

    /// Reset method to use.
    ///
    /// If unset, the reset defined by the target's debug sequence is used, which for
    /// most targets is a software-triggered system reset.
    #[clap(
        long,
        value_enum,
        ignore_case = true,
        default_value_t = ResetMethod::Default,
        env = "PROBE_RS_RESET_METHOD"
    )]
    method: ResetMethod,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        match self.method {
            // A pin reset acts on the probe, so it needs no target.
            ResetMethod::Hardware => {
                let probe = cli::select_probe(
                    &client,
                    self.common.probe.clone().map(to_wire_debug_probe_selector),
                    self.common.non_interactive,
                )
                .await?;

                client
                    .reset_hardware(ResetHardwareRequest {
                        probe,
                        speed: self.common.speed,
                        dry_run: self.common.dry_run,
                    })
                    .await?;
            }
            ResetMethod::Default => {
                let session = cli::attach_probe(&client, self.common, None, false).await?;
                session.core(self.shared.core).reset().await?;
            }
        }

        Ok(())
    }
}
