use crate::{
    rpc::{client::RpcClient, functions::flash::EraseCommand},
    util::{cli, common_options::ProbeOptions, flash::CliProgressBars},
};

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    common: ProbeOptions,

    #[arg(long, help_heading = "DOWNLOAD CONFIGURATION")]
    pub disable_progressbars: bool,

    /// Whether to read the RTT output from the flash loader, if available.
    #[clap(long)]
    pub read_flasher_rtt: bool,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let session = cli::attach_probe(&client, self.common, false).await?;

        let pb = if self.disable_progressbars {
            None
        } else {
            Some(CliProgressBars::new())
        };

        session
            .erase(
                EraseCommand::All,
                self.read_flasher_rtt,
                async move |event| {
                    if let Some(pb) = pb.as_ref() {
                        pb.handle(event);
                    }
                },
            )
            .await?;

        Ok(())
    }
}
