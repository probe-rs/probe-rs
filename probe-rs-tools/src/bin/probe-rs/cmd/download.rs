use std::path::PathBuf;

use crate::rpc::client::RpcClient;

use crate::FormatOptions;
use crate::util::cli;
use crate::util::common_options::BinaryDownloadOptions;
use crate::util::common_options::ProbeOptions;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    pub probe_options: ProbeOptions,

    /// The path to the file to be downloaded to the flash
    pub path: PathBuf,

    #[clap(flatten)]
    pub download_options: BinaryDownloadOptions,

    #[clap(flatten)]
    pub format_options: FormatOptions,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let session = cli::attach_probe(&client, self.probe_options, false).await?;

        cli::flash(
            &session,
            &self.path,
            self.download_options.chip_erase,
            self.format_options,
            self.download_options,
            None,
            None,
        )
        .await?;

        Ok(())
    }
}
