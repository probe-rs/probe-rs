use crate::{
    rpc::{
        Key,
        functions::{NoResponse, RpcContext, RpcResult},
    },
    util::rtt::{RttChannelConfig, RttConfig, client::RttClient},
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

    /// Default channel configuration.
    pub default_config: RttChannelConfig,
}

pub type RttClientKey = Key<RttClient>;

#[derive(Serialize, Deserialize, Schema)]
pub struct RttClientData {
    pub handle: RttClientKey,
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
        ScanRegion::Ranges(ranges) => {
            rtt::ScanRegion::Ranges(ranges.into_iter().map(|(start, end)| start..end).collect())
        }
        ScanRegion::Exact(addr) => rtt::ScanRegion::Exact(addr),
    };

    let client = RttClient::new(
        RttConfig {
            enabled: true,
            channels: request.config,
            default_config: request.default_config,
        },
        rtt_scan_regions,
        session.target(),
    );

    Ok(RttClientData {
        handle: ctx.store_object(client).await,
    })
}

#[derive(Serialize, Deserialize, Schema)]
pub struct RttDownRequest {
    pub sessid: Key<Session>,
    pub rtt_client: RttClientKey,
    pub channel: u32,
    pub data: Vec<u8>,
}

pub async fn write_rtt_down(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: RttDownRequest,
) -> NoResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut rtt_client = ctx.object_mut(request.rtt_client).await;

    let core_id = rtt_client.core_id();
    let mut core = session.core(core_id)?;
    rtt_client.write_down_channel(&mut core, request.channel, &request.data)?;

    Ok(())
}
