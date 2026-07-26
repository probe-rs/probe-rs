use crate::{
    CoreOptions,
    rpc::client::RpcClient,
    util::{
        cli,
        common_options::{ProbeOptions, RESET_HALT_TIMEOUT, ResetHaltOptions},
    },
};

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    common: ProbeOptions,

    #[clap(flatten)]
    reset: ResetHaltOptions,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let session = cli::attach_probe(&client, self.common, None, false).await?;
        let core = session.core(self.shared.core);

        if self.reset.reset_halt {
            core.reset_and_halt(RESET_HALT_TIMEOUT).await?;
        } else {
            core.reset().await?;
        }

        Ok(())
    }
}
