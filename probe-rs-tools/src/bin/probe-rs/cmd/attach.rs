use crate::rpc::client::RpcClient;
use crate::rpc::functions::monitor::{MonitorMode, MonitorOptions};
use crate::rpc::functions::rtt_client::LogOptions;
use crate::util::cli;

#[derive(clap::Parser)]
#[group(skip)]
pub struct Cmd {
    #[clap(flatten)]
    pub(crate) run: crate::cmd::run::Cmd,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let session =
            cli::attach_probe(&client, self.run.shared_options.probe_options, true).await?;

        let rtt = session
            .create_rtt_client(
                Some(self.run.shared_options.path.clone()),
                LogOptions {
                    no_location: self.run.shared_options.no_location,
                    log_format: self.run.shared_options.log_format,
                    rtt_scan_memory: self.run.shared_options.rtt_scan_memory,
                },
            )
            .await?;

        cli::monitor(
            &session,
            MonitorMode::AttachToRunning,
            &self.run.shared_options.path,
            MonitorOptions {
                catch_reset: self.run.run_options.catch_reset,
                catch_hardfault: self.run.run_options.catch_hardfault,
                rtt_client: Some(rtt),
            },
            self.run.shared_options.always_print_stacktrace,
        )
        .await?;

        Ok(())
    }
}
