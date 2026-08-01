use probe_rs_mi::info::BasicDeviceInfo;

use crate::{util::cli, util::common_options::ProbeOptions};
use probe_rs_rpc_client::RpcClient;

/// Attach to a probe and return the auto-detected chip name.
///
/// Any `--chip` override present in `probe` is cleared before attaching so
/// that the returned name always reflects what the probe discovers.
pub async fn basic_info(
    client: &RpcClient,
    mut probe: ProbeOptions,
) -> anyhow::Result<BasicDeviceInfo> {
    if probe.chip.is_some() {
        tracing::warn!("ignoring --chip option");
        probe.chip = None;
    }
    let session = cli::attach_probe(client, probe, None, false).await?;
    let metadata = session.target_metadata().await?;
    Ok(BasicDeviceInfo {
        chip: metadata.target_name,
    })
}
