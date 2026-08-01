use crate::{
    rpc::functions::{NoResponse, RpcContext, convert::lift},
    util::rtt::RttConfig,
};
use postcard_rpc::header::VarHeader;
use probe_rs::rtt;
pub use probe_rs_rpc::rtt_client::{
    CreateRttClientRequest, CreateRttClientResponse, PollRttUpRequest, PollRttUpResponse,
    RttChannelMeta, RttChannelRequest, RttChannels, RttChannelsResponse, RttClientData,
    RttDownRequest, RttPollResult, ScanRegion,
};

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

pub async fn write_rtt_down(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: RttDownRequest,
) -> NoResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut rtt_client = ctx.object_mut(request.rtt_client).await;

    let core_id = rtt_client.core_id();
    let mut core = lift(session.core(core_id))?;
    lift(rtt_client.write_down_channel(&mut core, request.channel, &request.data))?;

    Ok(())
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
