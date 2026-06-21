mod info;
mod meta;

use crate::rpc::client::RpcClient;

#[derive(clap::Parser)]
#[group(skip)]
pub struct Cmd {
    #[clap(subcommand)]
    subcommand: Subcommand,
}

#[derive(clap::Subcommand)]
#[group(skip)]
pub enum Subcommand {
    /// Print probe-rs version and build metadata as JSON.
    Meta,

    /// Print detected target device information as JSON.
    Info(info::Cmd),
}

impl Cmd {
    pub fn is_remote_cmd(&self) -> bool {
        // meta is not (yet) allowed, it just prints the client's info
        matches!(self.subcommand, Subcommand::Info(_))
    }

    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        match self.subcommand {
            Subcommand::Meta => meta::run()?,
            Subcommand::Info(cmd) => cmd.run(client).await?,
        }
        Ok(())
    }
}
