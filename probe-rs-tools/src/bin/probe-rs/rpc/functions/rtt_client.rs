use crate::{
    rpc::functions::{RpcContext, convert::lift},
    util::rtt::RttConfig,
};
use postcard_rpc::header::VarHeader;
use probe_rs::rtt;
use probe_rs_rpc::rtt_client::{
    CreateRttClientRequest, CreateRttClientResponse, PollRttUpRequest, PollRttUpResponse,
    RttChannelMeta, RttChannelRequest, RttChannels, RttChannelsResponse, RttClientData,
    RttDownRequest, RttDownResponse, RttPollResult, ScanRegion,
};
use probe_rs_rpc::{NoResponse, RpcError};
use std::time::{Duration, Instant};

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

    let core_id = client.core_id() as u32;
    Ok(RttClientData {
        handle: ctx.store_object(client).await,
        core_id,
    })
}

/// Upper bound on `RttDownRequest::timeout_ms`.
const MAX_DOWN_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait before retrying a full channel. The target drains on its own
/// schedule, so this only has to be short relative to that.
const DOWN_WRITE_RETRY_INTERVAL: Duration = Duration::from_millis(1);

pub async fn write_rtt_down(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: RttDownRequest,
) -> RttDownResponse {
    // Nothing to write
    if request.data.is_empty() {
        return Ok(0);
    }

    let timeout = Duration::from_millis(request.timeout_ms as u64).min(MAX_DOWN_WRITE_TIMEOUT);
    let deadline = Instant::now() + timeout;
    let mut written = 0;

    loop {
        let attached;

        // Scoped so the session and RTT client guards drop before we sleep
        {
            let mut session = ctx.session(request.sessid).await;
            let mut rtt_client = ctx.object_mut(request.rtt_client).await;

            let core_id = rtt_client.core_id();
            let mut core = lift(session.core(core_id))?;
            written += lift(rtt_client.write_down_channel(
                &mut core,
                request.channel,
                &request.data[written..],
            ))?;
            attached = rtt_client.is_attached();
        }

        // Nothing was written and nothing can be: report that rather than a
        // zero-byte write, which the caller cannot tell from a full channel.
        if !attached && written == 0 {
            return Err(RpcError::from("RTT is not attached"));
        }

        // Only a full buffer is worth waiting on. Writing while unattached scans
        // for the control block every time, so retrying that is far from free.
        // A partial write still returns its count: erroring would leave the caller
        // unable to tell what landed, and resending would duplicate it.
        if written == request.data.len() || !attached || Instant::now() >= deadline {
            return Ok(written as u32);
        }

        tokio::time::sleep(DOWN_WRITE_RETRY_INTERVAL).await;
    }
}

pub async fn get_rtt_channels(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: RttChannelRequest,
) -> RttChannelsResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut rtt_client = ctx.object_mut(request.rtt_client).await;

    let core_id = rtt_client.core_id();
    let mut core = lift(session.core(core_id))?;
    lift(rtt_client.try_attach(&mut core))?;

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

pub async fn clear_rtt_control_block(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: RttChannelRequest,
) -> NoResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut rtt_client = ctx.object_mut(request.rtt_client).await;

    let core_id = rtt_client.core_id();
    let mut core = lift(session.core(core_id))?;
    lift(rtt_client.clear_control_block(&mut core))?;

    Ok(())
}

pub async fn poll_rtt_up(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: PollRttUpRequest,
) -> PollRttUpResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut rtt_client = ctx.object_mut(request.rtt_client).await;

    let core_id = rtt_client.core_id();
    let mut core = lift(session.core(core_id))?;

    let mut results = Vec::with_capacity(request.channels.len());
    for channel in request.channels {
        let result = match rtt_client.poll_channel(&mut core, channel) {
            Ok(bytes) => Ok(bytes.to_vec()),
            Err(error) => {
                tracing::warn!("RTT poll of channel {channel} failed: {error}");
                Err(crate::rpc::functions::convert::rpc_error_rtt(error))
            }
        };
        results.push(RttPollResult { channel, result });
    }

    Ok(results)
}

pub async fn clean_up_rtt(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: RttChannelRequest,
) -> NoResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut rtt_client = ctx.object_mut(request.rtt_client).await;

    let core_id = rtt_client.core_id();
    let mut core = lift(session.core(core_id))?;
    lift(rtt_client.clean_up(&mut core))?;

    Ok(())
}
