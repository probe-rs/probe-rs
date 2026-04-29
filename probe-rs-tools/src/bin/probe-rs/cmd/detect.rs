use crate::{
    rpc::client::RpcClient,
    util::{cli, common_options::ProbeOptions},
};

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    common: ProbeOptions,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let session = cli::attach_probe(&client, self.common, false).await?;
        println!("{}", session.target_name());
        Ok(())
    }
}
