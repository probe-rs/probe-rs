use crate::{
    rpc::client::RpcClient,
    util::{cli, common_options::ProbeOptions},
};

/// Lock the debug port of the target device.
///
/// This command enables debug port protection on the target device, preventing
/// unauthorized debug access. The exact behavior is vendor-specific and may
/// require a power cycle to take effect.
///
/// Requires `--allow-permanent-debug-lock` to be passed as a safety measure.
#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    common: ProbeOptions,

    /// Lock level (vendor-specific). Omit to use the default level.
    #[arg(long)]
    pub level: Option<String>,

    /// List the supported lock levels for the connected device and exit.
    #[arg(long)]
    pub list_levels: bool,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let session = cli::attach_probe(&client, self.common, false).await?;

        if self.list_levels {
            println!("Available Lock Level:");
            let resp = session.supported_lock_levels().await?;
            for level in &resp.levels {
                println!(
                    "{}{}",
                    level.name,
                    if level.is_permanent {
                        " (permanent)"
                    } else {
                        ""
                    }
                );
                println!("  {}", level.description);
            }
            return Ok(());
        }

        session.lock_device(self.level).await?;
        println!(
            "Debug port locked successfully. A power cycle may be required for the lock to take effect."
        );
        Ok(())
    }
}
