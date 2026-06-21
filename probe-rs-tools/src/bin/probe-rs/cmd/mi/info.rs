use crate::{
    cmd::common::info::basic_info, rpc::client::RpcClient, util::common_options::ProbeOptions,
};

#[derive(clap::Parser)]
#[group(skip)]
pub struct Cmd {
    #[clap(flatten)]
    probe: ProbeOptions,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let info = basic_info(&client, self.probe).await?;
        println!("{}", serde_json::to_string(&info)?);
        Ok(())
    }
}
