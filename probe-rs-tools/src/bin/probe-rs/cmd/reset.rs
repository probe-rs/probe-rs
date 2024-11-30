use crate::{
    CoreOptions,
    rpc::client::RpcClient,
    util::{cli, common_options::ProbeOptions},
};

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    common: ProbeOptions,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let session = cli::attach_probe(&client, self.common, false).await?;
        let core = session.core(self.shared.core).await;

        core.reset().await?;

        Ok(())
    }
}
