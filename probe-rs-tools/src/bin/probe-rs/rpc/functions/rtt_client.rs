use crate::{
    rpc::{
        ConnectionState,
        functions::{
            RpcContext,
            convert::lift,
            write_queue::{QUEUE_DEPTH, accept},
        },
        utils::run_loop::DEFAULT_POLL_INTERVAL,
    },
    util::rtt::{RttConfig, client::RttClient, rtt_access},
};
use postcard_rpc::header::VarHeader;
use probe_rs::rtt;
use probe_rs_rpc::rtt_client::{
    CreateRttClientRequest, CreateRttClientResponse, PollRttUpRequest, PollRttUpResponse,
    RttChannelMeta, RttChannelRequest, RttChannels, RttChannelsResponse, RttClientData,
    RttDownRequest, RttDownResponse, RttPollResult, ScanRegion,
};
use probe_rs_rpc::{NoResponse, RpcError, RpcResult};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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

/// The RTT down channels of an RTT client while a run loop polls it.
pub(crate) struct RttDownStream {
    queue: mpsc::Sender<RttDownRequest>,
    /// The number of down channels. The poller sets it when RTT attaches.
    channels: OnceLock<usize>,
}

impl RttDownStream {
    /// Returns the stream, and the queue for [`write_queued`].
    pub(crate) fn new() -> (Self, mpsc::Receiver<RttDownRequest>) {
        let (queue, writes) = mpsc::channel(QUEUE_DEPTH);
        let stream = Self {
            queue,
            channels: OnceLock::new(),
        };
        (stream, writes)
    }

    pub(crate) fn set_channels(&self, channels: usize) {
        self.channels.get_or_init(|| channels);
    }

    fn accept(&self, request: RttDownRequest) -> RttDownResponse {
        let Some(&channels) = self.channels.get() else {
            return Err(RpcError::from("RTT is not attached"));
        };
        if request.channel as usize >= channels {
            return Err(RpcError::from(format!(
                "RTT down channel {} does not exist",
                request.channel
            )));
        }
        let len = request.data.len();
        accept(&self.queue, request, len)
    }
}

/// Writes the queued writes of an [`RttDownStream`] until `stop` is cancelled. A write waits for
/// a full channel until the target takes all of it.
pub(crate) async fn write_queued(
    state: ConnectionState,
    mut writes: mpsc::Receiver<RttDownRequest>,
    stop: CancellationToken,
) {
    while let Some(Some(request)) = stop.run_until_cancelled(writes.recv()).await {
        let mut written = 0;
        while written < request.data.len() {
            let Some(mut session) = stop
                .run_until_cancelled(state.session(request.sessid))
                .await
            else {
                return;
            };
            let mut rtt_client = state.object_mut(request.rtt_client).await;
            // The run loop can clean up RTT after the stop and before it releases the session.
            if stop.is_cancelled() {
                return;
            }
            let data = &request.data[written..];
            match write_down_once(&mut session, &mut rtt_client, request.channel, data) {
                Ok((count, true)) => written += count,
                // Writing while unattached scans for the control block every time.
                Ok((_, false)) => {
                    tracing::warn!(
                        "RTT is not attached, dropping a write to down channel {}",
                        request.channel
                    );
                    break;
                }
                Err(error) => {
                    tracing::warn!(
                        "Failed to write to RTT down channel {}: {error}",
                        request.channel
                    );
                    break;
                }
            }
            drop(rtt_client);
            drop(session);

            // A full channel waits for the target without limit. Each try uses the probe, so the
            // worker tries once for each pass of the run loop.
            if written < request.data.len()
                && stop
                    .run_until_cancelled(tokio::time::sleep(DEFAULT_POLL_INTERVAL))
                    .await
                    .is_none()
            {
                return;
            }
        }
    }
}

/// Writes to an RTT down channel. While a run loop polls the RTT client, the write goes to the
/// queue of the run loop, and the reply only says that the write is accepted.
pub async fn write_rtt_down(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: RttDownRequest,
) -> RttDownResponse {
    match ctx.state.rtt_down.get(request.rtt_client) {
        Some(stream) => stream.accept(request),
        None => write_rtt_down_now(&ctx.state, request).await,
    }
}

async fn write_rtt_down_now(state: &ConnectionState, request: RttDownRequest) -> RttDownResponse {
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
            let mut session = state.session(request.sessid).await;
            let mut rtt_client = state.object_mut(request.rtt_client).await;
            let (count, is_attached) = write_down_once(
                &mut session,
                &mut rtt_client,
                request.channel,
                &request.data[written..],
            )?;
            written += count;
            attached = is_attached;
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

/// Writes `data` once. Returns the number of bytes written and whether RTT is attached.
fn write_down_once(
    session: &mut probe_rs::Session,
    rtt_client: &mut RttClient,
    channel: u32,
    data: &[u8],
) -> RpcResult<(usize, bool)> {
    let mut core = lift(rtt_access(session, rtt_client))?;
    let count = lift(rtt_client.write_down_channel(&mut core, channel, data))?;
    Ok((count, rtt_client.is_attached()))
}

pub async fn get_rtt_channels(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: RttChannelRequest,
) -> RttChannelsResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut rtt_client = ctx.object_mut(request.rtt_client).await;

    let mut core = lift(rtt_access(&mut session, &mut rtt_client))?;
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

    let mut core = lift(rtt_access(&mut session, &mut rtt_client))?;
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

    let mut core = lift(rtt_access(&mut session, &mut rtt_client))?;

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

    let mut core = lift(rtt_access(&mut session, &mut rtt_client))?;
    lift(rtt_client.clean_up(&mut core))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use probe_rs_rpc::{Key, rtt_client::RttDownRequest};

    use super::RttDownStream;

    fn request(channel: u32) -> RttDownRequest {
        RttDownRequest {
            sessid: Key::new(),
            rtt_client: Key::new(),
            channel,
            data: vec![1, 2, 3],
            timeout_ms: 0,
        }
    }

    #[test]
    fn a_queued_down_write_needs_an_attached_channel() {
        let (stream, mut writes) = RttDownStream::new();
        assert!(stream.accept(request(0)).is_err());

        stream.set_channels(2);
        assert!(stream.accept(request(2)).is_err());
        assert_eq!(stream.accept(request(1)).unwrap(), 3);
        assert_eq!(writes.try_recv().unwrap().channel, 1);
    }
}
