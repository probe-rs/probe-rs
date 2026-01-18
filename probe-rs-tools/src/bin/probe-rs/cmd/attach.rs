use std::path::PathBuf;

use time::UtcOffset;

use crate::FormatOptions;
use crate::cmd::run::{MonitoringOptions, NormalRunOptions};
use crate::rpc::client::RpcClient;
use crate::rpc::functions::monitor::{MonitorMode, MonitorOptions};
use crate::util::cli::{self, connect_target_output_files, parse_semihosting_options, rtt_client};
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
            self.monitor_options.scan_region,
            self.monitor_options.log_format,
            !self.monitor_options.no_timestamps,
            !self.monitor_options.no_location,
            self.monitor_options.rtt_channel_mode,
            Some(utc_offset),
        )
        .await?;

        let mut target_output_files =
            connect_target_output_files(self.monitor_options.target_output_file).await?;

        let semihosting_options = parse_semihosting_options(self.monitor_options.semihosting_file)?;

        let client_handle = rtt_client.handle();

        cli::monitor(
            &session,
            MonitorMode::AttachToRunning,
            self.path.as_deref(),
            Some(rtt_client),
            MonitorOptions {
                catch_reset: !self.run_options.no_catch_reset,
                catch_hardfault: !self.run_options.no_catch_hardfault,
                rtt_client: Some(client_handle),
                semihosting_options,
            },
            self.monitor_options.rtt_down_channel,
            self.monitor_options.list_rtt,
            self.monitor_options.always_print_stacktrace,
            &mut target_output_files,
            self.monitor_options.stack_frame_limit,
        )
        .await?;

        Ok(())
    }
}
