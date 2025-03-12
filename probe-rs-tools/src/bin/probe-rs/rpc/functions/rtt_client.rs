use crate::{
    rpc::{
        Key,
        functions::{RpcContext, RpcResult},
    },
    util::rtt::{RttChannelConfig, RttConfig, client::ConfiguredRttClient},
};
use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use probe_rs::{Session, rtt};
use serde::{Deserialize, Serialize};

/// Used to specify which memory regions to scan for the RTT control block.
#[derive(Clone, Debug, Default, Serialize, Deserialize, Schema)]
pub enum ScanRegion {
    /// Scans all RAM regions known to probe-rs. This is the default and should always work, however
    /// if your device has a lot of RAM, scanning all of it is slow.
    #[default]
    Ram,

    /// Limit scanning to the memory addresses covered by the default region of the target.
    TargetDefault,

    /// Limit scanning to the memory addresses covered by all of the given ranges. It is up to the
    /// user to ensure that reading from this range will not read from undefined memory.
    Ranges(Vec<(u64, u64)>),

    /// Tries to find the control block starting at this exact address. It is up to the user to
    /// ensure that reading the necessary bytes after the pointer will no read from undefined
    /// memory.
    Exact(u64),
}

#[derive(Serialize, Deserialize, Schema)]
pub struct CreateRttClientRequest {
    pub sessid: Key<Session>,

    /// Scan the memory to find the RTT control block
    pub scan_regions: ScanRegion,

    /// Channel configuration.
    pub config: Vec<RttChannelConfig>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct RttClientData {
    pub handle: Key<ConfiguredRttClient>,
}

pub type CreateRttClientResponse = RpcResult<RttClientData>;

pub async fn create_rtt_client(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: CreateRttClientRequest,
) -> CreateRttClientResponse {
    let session = ctx.session(request.sessid).await;

    let rtt_scan_regions = match request.scan_regions {
        ScanRegion::Ram => rtt::ScanRegion::Ram,
        ScanRegion::TargetDefault => session.target().rtt_scan_regions.clone(),
        ScanRegion::Ranges(ranges) => {
            rtt::ScanRegion::Ranges(ranges.into_iter().map(|(start, end)| start..end).collect())
        }
        ScanRegion::Exact(addr) => rtt::ScanRegion::Exact(addr),
    };

    let client = ConfiguredRttClient::new(
        RttConfig {
            enabled: true,
            channels: request.config,
        },
        rtt_scan_regions,
    );

    Ok(RttClientData {
        handle: ctx.store_object(client).await,
    })
}
