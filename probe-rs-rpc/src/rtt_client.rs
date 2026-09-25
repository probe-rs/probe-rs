use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::rtt_config::RttChannelConfig;
use crate::{Key, RpcError, RpcResult, RttClient, Session};

#[derive(Clone, Debug, Default, Serialize, Deserialize, Schema)]
pub enum ScanRegion {
    #[default]
    Ram,
    Ranges(Vec<(u64, u64)>),
    Exact(u64),
}

#[derive(Serialize, Deserialize, Schema)]
pub struct CreateRttClientRequest {
    pub sessid: Key<Session>,
    pub scan_regions: ScanRegion,
    pub config: Vec<RttChannelConfig>,
    pub default_config: RttChannelConfig,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct RttClientData {
    pub handle: Key<RttClient>,
    /// Core that owns the RTT control block for this client.
    pub core_id: u32,
}

pub type CreateRttClientResponse = RpcResult<RttClientData>;

#[derive(Serialize, Deserialize, Schema)]
pub struct RttDownRequest {
    pub sessid: Key<Session>,
    pub rtt_client: Key<RttClient>,
    pub channel: u32,
    pub data: Vec<u8>,
    /// How long to keep retrying while the target's channel buffer is full.
    /// `0` means a single attempt. The server caps this.
    pub timeout_ms: u32,
}

/// The number of bytes the target accepted. A down channel write is not
/// blocking, so this may be less than the request's data length.
pub type RttDownResponse = RpcResult<u32>;

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

#[derive(Serialize, Deserialize, Schema)]
pub struct RttChannelRequest {
    pub sessid: Key<Session>,
    pub rtt_client: Key<RttClient>,
}

pub type PollRttUpResponse = RpcResult<Vec<RttPollResult>>;

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct RttPollResult {
    pub channel: u32,
    pub result: Result<Vec<u8>, RpcError>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct PollRttUpRequest {
    pub sessid: Key<Session>,
    pub rtt_client: Key<RttClient>,
    pub channels: Vec<u32>,
}
