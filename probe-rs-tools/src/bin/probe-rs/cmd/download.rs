use std::path::PathBuf;

use probe_rs_rpc_client::RpcClient;

use crate::util::cli;
use crate::util::common_options::BinaryDownloadOptions;
use crate::util::common_options::ProbeOptions;
use probe_rs_rpc::format::FormatOptions;

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

    /// Start the firmware after the download.
    ///
    /// The target gets a reset only if the firmware needs one to start.
    #[clap(long, help_heading = "DOWNLOAD CONFIGURATION")]
    pub start: bool,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let session = cli::attach_probe(&client, self.probe_options, None, false).await?;

        let boot_info = cli::flash(
            &session,
            &self.path,
            self.format_options,
            self.download_options,
            None,
            None,
        )
        .await?;

        if self.start {
            session.boot(boot_info, 0).await?;
        } else if cli::is_armv4t_target(&session).await? {
            // ARM7TDMI only: without `--start`, the flash algorithm's teardown leaves the core
            // halted at the algorithm's load address, and the ARM7 session detach resumes it
            // from there - running whatever is left of the algorithm's RAM image. A reset leaves
            // the target in a clean, known state instead. Other architectures keep the
            // "no `--start` means no reset" behaviour, which some targets (e.g. FPGA soft cores)
            // depend on.
            session.core(0).reset().await?;
        }

        Ok(())
    }
}
