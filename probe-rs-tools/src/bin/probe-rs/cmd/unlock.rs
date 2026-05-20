use crate::{
    rpc::client::RpcClient,
    util::{cli, common_options::ProbeOptions},
};

/// Unlock the debug port of the target device.
///
/// This command unlocks the target device.
/// The exact behavior is vendor-specific and may require a power cycle to take effect.
///
/// Might require `--allow-erase-all` to be passed in case some devices use mass erase
/// to debug unlock.
#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    common: ProbeOptions,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        // We simply attach a probe, which calls `debug_device_unlock`,
        let _ = cli::attach_probe(&client, self.common, false).await?;
        println!(
            "Debug port unlocked successfully. A power cycle may be required for the lock to take effect."
        );
        Ok(())
    }
}
