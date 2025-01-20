use crate::rpc::client::RpcClient;

#[derive(clap::Parser)]
pub struct Cmd {}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let probes = client.list_probes().await?;

        if !probes.is_empty() {
            println!("The following debug probes were found:");
            for (num, link) in probes.iter().enumerate() {
                println!("[{num}]: {link}");
            }
        } else {
            println!("No debug probes were found.");
        }
        Ok(())
    }
}
