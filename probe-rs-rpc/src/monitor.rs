use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::core_ops::WireHaltReason;
use crate::flash::BootInfo;
use crate::semihosting_options::SemihostingOptions;
use crate::{Key, RpcResult, RttClient, Session};

#[derive(Serialize, Deserialize, Schema)]
pub enum MonitorMode {
    AttachToRunning,
    Run(BootInfo),
}

impl MonitorMode {
    pub fn should_clear_rtt_header(&self) -> bool {
        match self {
            MonitorMode::Run(BootInfo::FromRam { .. }) => true,
            MonitorMode::Run(BootInfo::Other) => true,
            MonitorMode::AttachToRunning => false,
        }
    }
}

#[derive(Serialize, Deserialize, Schema)]
pub struct MonitorOptions {
    pub catch_reset: bool,
    pub catch_hardfault: bool,
    pub catch_svc: bool,
    pub catch_hlt: bool,
    pub rtt_client: Option<Key<RttClient>>,
    pub semihosting_options: SemihostingOptions,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct MonitorRequest {
    pub sessid: Key<Session>,
    pub mode: MonitorMode,
    pub options: MonitorOptions,
}

#[derive(Serialize, Deserialize, Schema)]
pub enum MonitorExitReason {
    UserExit,
    SemihostingExit(Result<(), SemihostingExitError>),
    /// The core halted, and the run loop did not handle the halt. The client
    /// decides what the halt means for the user.
    Halted(WireHaltReason),
}

#[derive(Serialize, Deserialize, Schema)]
pub struct SemihostingExitError {
    pub reason: u32,
    pub subcode: Option<u32>,
}

pub type MonitorResponse = RpcResult<MonitorExitReason>;

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct ChannelInfo {
    pub name: String,
    pub buffer_size: u64,
}

#[derive(Serialize, Deserialize, Schema)]
pub enum RttEvent {
    Discovered {
        up_channels: Vec<ChannelInfo>,
        down_channels: Vec<ChannelInfo>,
    },
    Output {
        channel: u32,
        bytes: Vec<u8>,
    },
}

#[derive(Serialize, Deserialize, Schema)]
pub enum SemihostingEvent {
    Output { stream: String, data: String },
}
