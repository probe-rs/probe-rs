use std::path::PathBuf;

use time::UtcOffset;

use crate::FormatOptions;
use crate::cmd::run::{MonitoringOptions, NormalRunOptions};
use crate::rpc::client::RpcClient;
use crate::rpc::functions::monitor::MonitorMode;
use crate::rpc::utils::run_loop::VectorCatchConfig;
use crate::util::cli::{self, rtt_client};
use crate::util::common_options::ProbeOptions;

#[derive(clap::Parser)]
#[group(skip)]
pub struct Cmd {
    /// Options only used when in normal run mode
    #[clap(flatten)]
    pub(crate) run_options: NormalRunOptions,

    #[clap(flatten)]
    pub(crate) probe_options: ProbeOptions,

    /// The path to the ELF file to flash and run.
    #[clap(index = 1)]
    pub(crate) path: Option<PathBuf>,

    #[clap(flatten)]
    pub(crate) format_options: FormatOptions,

    #[clap(flatten)]
    pub(crate) monitor_options: MonitoringOptions,
}

impl Cmd {
    pub async fn run(self, client: RpcClient, utc_offset: UtcOffset) -> anyhow::Result<()> {
        let session = cli::attach_probe(&client, self.probe_options, true).await?;

        let rtt_client = rtt_client(
            &session,
            self.path.as_deref(),
            &self.monitor_options,
            Some(utc_offset),
        )
        .await?;

        cli::monitor(
            &session,
            MonitorMode::AttachToRunning,
            self.path.as_deref(),
            &self.monitor_options,
            Some(rtt_client),
            VectorCatchConfig {
                catch_hardfault: !self.run_options.no_catch_hardfault,
                catch_reset: !self.run_options.no_catch_reset,
                catch_svc: !self.run_options.no_catch_svc,
                catch_hlt: !self.run_options.no_catch_hlt,
            },
        )
        .await?;

        Ok(())
    }
}
