use time::UtcOffset;

use crate::rpc::client::RpcClient;
use crate::rpc::functions::monitor::{MonitorMode, MonitorOptions};
use crate::util::cli::{self, rtt_client};

#[derive(clap::Parser)]
#[group(skip)]
pub struct Cmd {
    #[clap(flatten)]
    pub(crate) run: crate::cmd::run::Cmd,
}

impl Cmd {
    pub async fn run(self, client: RpcClient, utc_offset: UtcOffset) -> anyhow::Result<()> {
        let session =
            cli::attach_probe(&client, self.run.shared_options.probe_options, true).await?;

        let rtt_client = rtt_client(
            &session,
            &self.run.shared_options.path,
            match self.run.shared_options.rtt_scan_memory {
                true => crate::rpc::functions::rtt_client::ScanRegion::TargetDefault,
                false => crate::rpc::functions::rtt_client::ScanRegion::Ranges(vec![]),
            },
            self.run.shared_options.log_format,
            !self.run.shared_options.no_location,
            Some(utc_offset),
        )
        .await?;

        let client_handle = rtt_client.handle();

        cli::monitor(
            &session,
            MonitorMode::AttachToRunning,
            &self.run.shared_options.path,
            Some(rtt_client),
            MonitorOptions {
                catch_reset: self.run.run_options.catch_reset,
                catch_hardfault: self.run.run_options.catch_hardfault,
                rtt_client: Some(client_handle),
            },
            self.run.shared_options.always_print_stacktrace,
        )
        .await?;

        Ok(())
    }
}
