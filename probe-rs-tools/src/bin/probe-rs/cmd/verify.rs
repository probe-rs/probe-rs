use std::path::PathBuf;

use crate::FormatOptions;
use crate::rpc::client::RpcClient;
use crate::rpc::functions::flash::VerifyResult;
use crate::util::cli;
use crate::util::common_options::ProbeOptions;
use crate::util::flash::CliProgressBars;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    pub probe_options: ProbeOptions,

    /// The path to the file to be compared with the flash
    pub path: PathBuf,

    #[clap(flatten)]
    pub format_options: FormatOptions,

    #[clap(long)]
    pub disable_progressbars: bool,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let session = cli::attach_probe(&client, self.probe_options, false).await?;

        let pb = if self.disable_progressbars {
            None
        } else {
            Some(CliProgressBars::new())
        };
        let loader = session
            .build_flash_loader(self.path.to_path_buf(), self.format_options, None)
            .await?;

        let result = session
            .verify(loader.loader, async move |event| {
                if let Some(pb) = pb.as_ref() {
                    pb.handle(event);
                }
            })
            .await?;

        match result {
            VerifyResult::Ok => println!("Verification successful"),
            VerifyResult::Mismatch => println!("Verification failed: contents do not match"),
        }

        Ok(())
    }
}
