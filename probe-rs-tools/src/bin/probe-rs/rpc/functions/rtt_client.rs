use crate::{
    rpc::{
        Key, RttClient, Session,
        functions::{NoResponse, RpcContext, RpcError, RpcResult},
    },
    util::rtt::{RttChannelConfig, RttConfig},
};
use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use probe_rs::rtt;
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

#[derive(Serialize, Deserialize, Schema)]
pub struct RttClientData {
    pub handle: Key<RttClient>,
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

    let client = crate::util::rtt::client::RttClient::new(
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
    pub rtt_client: Key<RttClient>,
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

/// Metadata for a single RTT up/down channel, returned by [`get_rtt_channels`].
#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct RttChannelMeta {
    pub number: u32,
    pub name: String,
}

#[derive(Serialize, Deserialize, Schema, Clone, Default)]
pub struct RttChannels {
    pub up: Vec<RttChannelMeta>,
    pub down: Vec<RttChannelMeta>,
}

pub type RttChannelsResponse = RpcResult<RttChannels>;

/// Attach the server-side [`RttClient`] to the target's RTT control block
/// (if not already attached) and return the up/down channel metadata.
pub async fn get_rtt_channels(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: RttChannelRequest,
) -> RttChannelsResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut rtt_client = ctx.object_mut(request.rtt_client).await;

    let core_id = rtt_client.core_id();
    let mut core = session.core(core_id)?;
    rtt_client.try_attach(&mut core)?;

    let up = rtt_client
        .up_channels()
        .iter()
        .map(|c| RttChannelMeta {
            number: c.number(),
            name: c.channel_name(),
        })
        .collect();
    let down = rtt_client
        .down_channels()
        .iter()
        .map(|c| RttChannelMeta {
            number: c.number(),
            name: c.channel_name(),
        })
        .collect();

    Ok(RttChannels { up, down })
}

/// Wipe any stale RTT control block from target memory. Called while the core
/// is halted, before a reset or reflash, so firmware startup reinitializes the
/// block from `.data`.
///
/// Delegates to the stored [`RttClient`], so this honours the
/// "control block is initialized by the flash loader" guard that
/// [`RttClient::configure_from_loader`] sets, and reuses the client's scan
/// region and cached control block address.
pub async fn clear_rtt_control_block(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: RttChannelRequest,
) -> NoResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut rtt_client = ctx.object_mut(request.rtt_client).await;

    let core_id = rtt_client.core_id();
    let mut core = session.core(core_id)?;
    rtt_client.clear_control_block(&mut core)?;

    Ok(())
}

#[derive(Serialize, Deserialize, Schema)]
pub struct RttChannelRequest {
    pub sessid: Key<Session>,
    pub rtt_client: Key<RttClient>,
}

pub type PollRttUpResponse = RpcResult<Vec<RttPollResult>>;

/// Result of polling a single up channel in a batched [`poll_rtt_up`] request.
///
/// `result` is `Ok(data)` when the channel was polled successfully (empty
/// `data` means no new bytes, including the recoverable corrupted-control-
/// block case which `RttClient::poll_channel` handles internally), and
/// `Err(message)` when the channel could not be polled at all (e.g. the
/// probe was lost).
#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct RttPollResult {
    pub channel: u32,
    pub result: Result<Vec<u8>, RpcError>,
}

/// Poll multiple up channels on the server-side [`RttClient`] in a single
/// request, returning the newly-available bytes for each.
pub async fn poll_rtt_up(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: PollRttUpRequest,
) -> PollRttUpResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut rtt_client = ctx.object_mut(request.rtt_client).await;

    let core_id = rtt_client.core_id();
    let mut core = session.core(core_id)?;

    let mut results = Vec::with_capacity(request.channels.len());
    for channel in request.channels {
        let result = match rtt_client.poll_channel(&mut core, channel) {
            Ok(bytes) => Ok(bytes.to_vec()),
            Err(error) => {
                tracing::warn!("RTT poll of channel {channel} failed: {error}");
                Err(RpcError::from(error))
            }
        };
        results.push(RttPollResult { channel, result });
    }

    Ok(results)
}

#[derive(Serialize, Deserialize, Schema)]
pub struct PollRttUpRequest {
    pub sessid: Key<Session>,
    pub rtt_client: Key<RttClient>,
    /// Up channel numbers to poll in this request.
    pub channels: Vec<u32>,
}

/// Restore the original mode of every up channel on the server-side
/// [`RttClient`].
pub async fn clean_up_rtt(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: RttChannelRequest,
) -> NoResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut rtt_client = ctx.object_mut(request.rtt_client).await;

    let core_id = rtt_client.core_id();
    let mut core = session.core(core_id)?;
    rtt_client.clean_up(&mut core)?;

    Ok(())
}
