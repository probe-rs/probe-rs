use crate::{
    CoreOptions,
    util::{cli, common_options::ProbeOptions},
};
use probe_rs_rpc_client::RpcClient;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    common: ProbeOptions,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let session = cli::attach_probe(&client, self.common, None, false).await?;
        let core = session.core(self.shared.core);

        core.reset().await?;

        Ok(())
    }
}
