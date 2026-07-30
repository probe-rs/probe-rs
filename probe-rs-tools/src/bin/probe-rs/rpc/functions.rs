use std::{any::Any, ops::DerefMut, sync::Arc};
use std::{collections::HashMap, convert::Infallible, future::Future};

use crate::rpc::SessionState;
use crate::rpc::debug_state::{CoreDebugState, ServerDebugState};
use crate::rpc::functions::file::{
    AppendFileRequest, CreateFileResponse, append_temp_file, create_temp_file,
};
use crate::{
    rpc::{
        ConnectionState, Key,
        functions::{
            breakpoints::{
                ResolveSourceBreakpointsRequest, ResolveSourceLocationsRequest,
                resolve_source_breakpoints, resolve_source_locations,
            },
            chip::{
                ChipInfoRequest, ChipInfoResponse, ListFamiliesResponse, LoadChipFamilyRequest,
                chip_info, list_families, load_chip_family,
            },
            core_ops::{
                CoreAccessRequest, CoreBreakpointsRequest, CoreDumpRequest, CoreHaltRequest,
                CoreReadRegistersRequest, CoreVectorCatchRequest, CoreWriteRegRequest,
                HandleSemihostingRequest, StepRequest, WireCoreDump, WireCoreInformation,
                WireCoreMetadata, WireCoreStatus, WireRegisterReadResult, core_clear_hw_bps,
                core_dump, core_enable_vc, core_halt, core_handle_semihosting, core_metadata,
                core_read_registers, core_run, core_set_hw_bps, core_status, core_step,
                core_write_reg,
            },
            debug_vars::{
                ClearCoreDebugStateRequest, EvaluateRequest, LoadSvdRequest, ScopesRequest,
                SetVariableRequest, VariablesRequest, clear_core_debug_state,
                evaluate as debug_evaluate, load_svd as debug_load_svd, scopes as debug_scopes,
                set_variable as debug_set_variable, variables as debug_variables,
            },
            disassemble::{DisassembleRequest, disassemble as disassemble_handler},
            flash::{
                BuildRequest, BuildResponse, EraseRequest, FlashRequest, ProgressEvent,
                VerifyRequest, VerifyResponse, build, erase, flash, verify,
            },
            info::{
                InfoEvent, TargetInfoRequest, TargetMetadataRequest, target_info, target_metadata,
            },
            memory::{
                ReadBytesRequest, ReadMemoryRequest, WriteMemoryRequest, read_bytes, read_memory,
                write_memory,
            },
            monitor::{MonitorRequest, MonitorResponse, RttEvent, SemihostingEvent, monitor},
            probe::{
                AttachRequest, AttachResponse, ListProbesResponse, SelectProbeRequest,
                SelectProbeResponse, attach, list_probes, select_probe,
            },
            reset::{ResetCoreAndHaltRequest, ResetCoreRequest, reset, reset_and_halt},
            resume::{ResumeAllCoresRequest, resume_all_cores},
            rtt_client::{
                CreateRttClientRequest, CreateRttClientResponse, PollRttUpRequest,
                PollRttUpResponse, RttChannelRequest, RttChannelsResponse, RttDownRequest,
                clean_up_rtt, clear_rtt_control_block, create_rtt_client, get_rtt_channels,
                poll_rtt_up, write_rtt_down,
            },
            stack_trace::{
                LoadDebugInfoRequest, LoadDebugInfoResponse, TakeRichStackTraceRequest,
                TakeRichStackTraceResponse, TakeStackTraceRequest, TakeStackTraceResponse,
                load_debug_info, take_rich_stack_trace, take_stack_trace,
            },
            test::{
                ListTestsRequest, ListTestsResponse, RunTestRequest, RunTestResponse,
                TestKickoffRequest, TestKickoffResponse, list_tests, run_test, test_kickoff,
            },
        },
        transport::memory::{WireRx, WireTx},
    },
    util::common_options::OperationError,
};

use anyhow::anyhow;
use postcard_rpc::header::{VarHeader, VarSeq};
use postcard_rpc::server::{
    Dispatch, Sender as PostcardSender, Server, SpawnContext, WireRxErrorKind, WireTxErrorKind,
};
use postcard_rpc::{Topic, TopicDirection, endpoints, host_client, server, topics};
use postcard_schema::Schema;
use probe_rs::config::Registry;
use probe_rs::integration::ProbeLister;
use probe_rs::probe::list::{AllProbesLister, ProbeListItem};
use probe_rs::probe::{DebugProbeError, DebugProbeSelector, Probe, ProbeCreationError};
use probe_rs::{Session, probe::list::Lister};
use serde::{Deserialize, Serialize};
use tokio::sync::{
    Mutex,
    mpsc::{Receiver, Sender, channel},
};
use tokio_util::sync::CancellationToken;

pub mod breakpoints;
pub mod chip;
pub mod core_ops;
pub mod debug_vars;
pub mod disassemble;
pub mod file;
pub mod flash;
pub mod info;
pub mod memory;
pub mod monitor;
pub mod probe;
pub mod reset;
pub mod resume;
pub mod rtt_client;
pub mod stack_trace;
pub mod test;

pub type RpcResult<T> = Result<T, RpcError>;

pub type NoResponse = RpcResult<()>;

#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct RpcError(String);

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// TODO: replace most of these with anyhow context wrappers
impl From<&str> for RpcError {
    fn from(e: &str) -> Self {
        Self(e.to_string())
    }
}

impl From<String> for RpcError {
    fn from(e: String) -> Self {
        Self(e)
    }
}

impl From<anyhow::Error> for RpcError {
    fn from(e: anyhow::Error) -> Self {
        Self(format!("{e:?}"))
    }
}

impl From<probe_rs::Error> for RpcError {
    fn from(e: probe_rs::Error) -> Self {
        Self::from(anyhow!(e))
    }
}

impl From<probe_rs_debug::DebugError> for RpcError {
    fn from(e: probe_rs_debug::DebugError) -> Self {
        Self::from(anyhow!(e))
    }
}

impl From<probe_rs::flashing::FileDownloadError> for RpcError {
    fn from(e: probe_rs::flashing::FileDownloadError) -> Self {
        Self::from(anyhow!(e))
    }
}

impl From<probe_rs::flashing::FlashError> for RpcError {
    fn from(e: probe_rs::flashing::FlashError) -> Self {
        Self::from(anyhow!(e))
    }
}

impl From<probe_rs::config::RegistryError> for RpcError {
    fn from(e: probe_rs::config::RegistryError) -> Self {
        Self::from(anyhow!(e))
    }
}

impl From<OperationError> for RpcError {
    fn from(e: OperationError) -> Self {
        Self::from(anyhow!(e))
    }
}

impl From<probe_rs::rtt::Error> for RpcError {
    fn from(e: probe_rs::rtt::Error) -> Self {
        Self::from(anyhow!(e))
    }
}

impl From<WireTxErrorKind> for RpcError {
    fn from(e: WireTxErrorKind) -> Self {
        Self(format!("{e:?}"))
    }
}

impl From<RpcError> for anyhow::Error {
    fn from(e: RpcError) -> Self {
        anyhow!(e.0)
    }
}

#[derive(Clone)]
pub struct RpcSpawnContext {
    state: ConnectionState,
    sender: PostcardSender<WireTxImpl>,
}

pub(crate) trait MultiTopicWriter {
    type Sender: Send + 'static;
    type Publisher: MultiTopicPublisher;

    fn create(token: CancellationToken) -> (Self::Sender, Self::Publisher);
}

impl<T> MultiTopicWriter for T
where
    T: Topic,
    T::Message: Serialize + Sized + Send + 'static,
{
    type Sender = Sender<T::Message>;
    type Publisher = TopicPublisher<T>;

    fn create(token: CancellationToken) -> (Self::Sender, Self::Publisher) {
        let (tx, rx) = channel::<T::Message>(256);
        (tx, TopicPublisher { rx, token })
    }
}

pub(crate) trait MultiTopicPublisher {
    async fn publish(self, sender: &PostcardSender<WireTxImpl>);
}

pub(crate) struct TopicPublisher<T>
where
    T: Topic,
    T::Message: Serialize + Sized + Send + 'static,
{
    rx: Receiver<T::Message>,
    token: CancellationToken,
}

impl<T> MultiTopicPublisher for TopicPublisher<T>
where
    T: Topic,
    T::Message: Serialize + Sized + Send + 'static,
{
    async fn publish(mut self, sender: &PostcardSender<WireTxImpl>) {
        loop {
            tokio::select! {
                biased;

                _ = self.token.cancelled() => break,
                Some(event) = self.rx.recv() => {
                    sender
                        .publish::<T>(VarSeq::Seq2(0), &event)
                        .await
                        .unwrap();
                }
            }
        }
        std::mem::drop(self.rx);

        futures_util::future::pending().await
    }
}

impl RpcSpawnContext {
    fn dry_run(&self, sessid: Key<Session>) -> bool {
        self.shared_session(sessid).dry_run()
    }

    fn session_blocking(&self, sessid: Key<Session>) -> impl DerefMut<Target = Session> + use<> {
        self.shared_session(sessid).session_blocking()
    }

    fn shared_session(&self, sessid: Key<Session>) -> SessionState<'_> {
        self.state.shared_session(sessid)
    }

    pub fn object_mut_blocking<T: Any + Send>(
        &self,
        key: Key<T>,
    ) -> impl DerefMut<Target = T> + Send + use<T> {
        self.state.object_mut_blocking(key)
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.state.token.clone()
    }

    pub async fn run_blocking<T, F, REQ, RESP>(&mut self, request: REQ, task: F) -> RESP
    where
        T: MultiTopicWriter,
        F: FnOnce(RpcSpawnContext, REQ, T::Sender) -> RESP,
        F: Send + 'static,
        REQ: Send + 'static,
        RESP: Send + 'static,
    {
        let token = self.cancellation_token();
        let (sender, publisher) = T::create(token);

        let ctx = self.clone();
        let blocking = tokio::task::spawn_blocking(move || task(ctx, request, sender));

        tokio::select! {
            _ =  publisher.publish(&self.sender) => unreachable!(),
            response = blocking => {
                response.unwrap()
            }
        }
    }
}

/// Struct to list all attached debug probes
#[derive(Debug)]
pub struct LimitedLister {
    all_probes: AllProbesLister,
    probe_access: ProbeAccess,
}

impl LimitedLister {
    pub fn new(probe_access: ProbeAccess) -> Self {
        Self {
            all_probes: AllProbesLister::new(),
            probe_access,
        }
    }

    fn is_allowed(&self, selector: &DebugProbeSelector) -> bool {
        // We aren't using `.to_string()` because it doesn't append an empty serial when missing.
        let sel_without_serial = format!("{:04x}:{:04x}", selector.vendor_id, selector.product_id);
        let mut sel_with_serial = format!("{sel_without_serial}:");
        if let Some(sn) = selector.serial_number.as_deref() {
            sel_with_serial.push_str(sn);
        }

        let matching = |s: &String| s == &sel_with_serial || s == &sel_without_serial;

        match &self.probe_access {
            ProbeAccess::All => true,
            ProbeAccess::Allow(allow) => allow.iter().any(matching),
            ProbeAccess::Deny(deny) => !deny.iter().any(matching),
        }
    }
}

impl ProbeLister for LimitedLister {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Probe, DebugProbeError> {
        if !self.is_allowed(selector) {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::CouldNotOpen,
            ));
        }
        self.all_probes.open(selector)
    }

    fn list_with_access(&self, selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem> {
        self.all_probes
            .list_with_access(selector)
            .into_iter()
            .filter(|item| self.is_allowed(&DebugProbeSelector::from(&item.info)))
            .collect()
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
//#[serde(rename_all = "snake_case", tag = "type", content = "probes")]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProbeAccess {
    #[default]
    All,
    Allow(Vec<String>),
    Deny(Vec<String>),
}

pub struct RpcContext {
    /// State associated with a single connection.
    state: ConnectionState,
    sender: Option<PostcardSender<WireTxImpl>>,
    /// Probe lister shared with the dispatch handlers. Stored as
    /// `Arc<dyn ProbeLister + Send + Sync>` so [`RpcContext`] stays
    /// `Send + Sync` (the server future is driven via `tokio::spawn`).
    /// [`RpcContext::lister`] repackages it as an owned [`Lister`] per call.
    lister: Arc<dyn ProbeLister + Send + Sync>,
}

/// Shim that lets a [`Lister`] own a reference to the shared
/// `Arc<dyn ProbeLister + Send + Sync>` stored in [`RpcContext`].
#[derive(Debug)]
struct ArcLister(Arc<dyn ProbeLister + Send + Sync>);

impl ProbeLister for ArcLister {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Probe, DebugProbeError> {
        self.0.open(selector)
    }
    fn list_with_access(&self, selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem> {
        self.0.list_with_access(selector)
    }
}

impl SpawnContext for RpcContext {
    type SpawnCtxt = RpcSpawnContext;

    fn spawn_ctxt(&mut self) -> Self::SpawnCtxt {
        self.state.token = CancellationToken::new();
        RpcSpawnContext {
            state: self.state.clone(),
            sender: self.sender.clone().unwrap(),
        }
    }
}

impl RpcContext {
    /// Build a context with a custom probe lister, bypassing the
    /// [`ProbeAccess`] filtering applied by [`LimitedLister`]. Used by tests
    /// that drive the in-process RPC server with a `FakeProbe`.
    pub fn with_lister(lister: Arc<dyn ProbeLister + Send + Sync>) -> Self {
        Self {
            state: ConnectionState::new(),
            sender: None,
            lister,
        }
    }

    pub fn set_sender(&mut self, sender: PostcardSender<WireTxImpl>) {
        self.sender = Some(sender);
    }

    pub async fn publish<T>(&self, seq_no: VarSeq, msg: &T::Message) -> anyhow::Result<()>
    where
        T: ?Sized,
        T: Topic,
        T::Message: Serialize + Schema,
    {
        self.sender
            .as_ref()
            .unwrap()
            .publish::<T>(seq_no, msg)
            .await
            .map_err(|e| anyhow!("{e:?}"))
    }

    pub async fn object_mut<T: Any + Send>(
        &self,
        key: Key<T>,
    ) -> impl DerefMut<Target = T> + Send + use<T> {
        self.state.object_mut(key).await
    }

    pub async fn store_object<T: Any + Send>(&mut self, obj: T) -> Key<T> {
        self.state.store_object(obj).await
    }

    pub async fn set_session(&mut self, session: Session, dry_run: bool) -> Key<Session> {
        self.state.set_session(session, dry_run).await
    }

    pub async fn session(
        &self,
        sid: Key<Session>,
    ) -> impl DerefMut<Target = Session> + Send + use<> {
        self.object_mut(sid).await
    }

    pub fn debug_states(&self) -> DebugStatesMap {
        self.state.debug_states.clone()
    }

    /// Run `f` against the session's debug state, creating an empty state on
    /// first use. A session attached without a program binary has no DWARF,
    /// but still owns SVD and semihosting state, so the state itself always
    /// exists; only [`ServerDebugState::debug_info`] is optional.
    pub async fn with_server_debug_state<R>(
        &self,
        sessid: Key<Session>,
        f: impl FnOnce(&ServerDebugState) -> R,
    ) -> R {
        let states = self.debug_states();
        let mut guard = states.lock().await;
        f(guard.entry(sessid).or_default())
    }

    pub async fn with_server_debug_state_mut<R>(
        &self,
        sessid: Key<Session>,
        f: impl FnOnce(&mut ServerDebugState) -> R,
    ) -> R {
        let states = self.debug_states();
        let mut guard = states.lock().await;
        f(guard.entry(sessid).or_default())
    }

    pub async fn with_core_debug_state_mut<R>(
        &self,
        sessid: Key<Session>,
        core: u32,
        f: impl FnOnce(&mut CoreDebugState) -> R,
    ) -> Result<R, &'static str> {
        let states = self.debug_states();
        let mut guard = states.lock().await;
        let state = guard.get_mut(&sessid).ok_or("No debug state for session")?;
        let core_state = state
            .per_core
            .get_mut(&(core as usize))
            .ok_or("No debug state for core")?;
        Ok(f(core_state))
    }

    pub fn lister(&self) -> Lister {
        Lister::with_lister(Box::new(ArcLister(self.lister.clone())))
    }

    pub async fn registry(&self) -> impl DerefMut<Target = Registry> + Send + use<> {
        self.state.registry.clone().lock_owned().await
    }

    pub async fn run_blocking<T, F, REQ, RESP>(&mut self, request: REQ, task: F) -> RESP
    where
        T: Topic,
        T::Message: Serialize + Schema + Sized + Send + 'static,
        F: FnOnce(RpcSpawnContext, REQ, Sender<T::Message>) -> RESP,
        F: Send + 'static,
        REQ: Send + 'static,
        RESP: Send + 'static,
    {
        self.spawn_ctxt()
            .run_blocking::<T, F, REQ, RESP>(request, task)
            .await
    }
}

async fn cancel_handler(
    ctx: &mut RpcContext,
    _header: VarHeader,
    _msg: (),
    _sender: &PostcardSender<WireTxImpl>,
) {
    ctx.state.token.cancel();
}

#[derive(Clone)]
pub struct TokioSpawner;

impl server::WireSpawn for TokioSpawner {
    type Error = std::convert::Infallible;
    type Info = ();

    fn info(&self) -> &Self::Info {
        &()
    }
}
impl host_client::WireSpawn for TokioSpawner {
    fn spawn(&mut self, fut: impl Future<Output = ()> + Send + 'static) {
        _ = tokio::spawn(fut);
    }
}

pub fn spawn_fn(
    _sp: &TokioSpawner,
    fut: impl Future<Output = ()> + 'static + Send,
) -> Result<(), Infallible> {
    tokio::task::spawn(fut);
    Ok(())
}

pub(crate) type DebugStatesMap = Arc<Mutex<HashMap<Key<Session>, ServerDebugState>>>;

type TargetMetadataResponse = RpcResult<info::WireSessionTargetMetadata>;

type ReadMemory8Response = RpcResult<Vec<u8>>;
type ReadMemory16Response = RpcResult<Vec<u16>>;
type ReadMemory32Response = RpcResult<Vec<u32>>;
type ReadMemory64Response = RpcResult<Vec<u64>>;
type ReadBytesResponse = RpcResult<Vec<u8>>;

type ScopesResponse = debug_vars::ScopesResponse;
type VariablesResponse = debug_vars::VariablesResponse;
type EvaluateResponse = debug_vars::EvaluateResponse;
type SetVariableResponse = debug_vars::SetVariableResult;
type LoadSvdResponse = debug_vars::LoadSvdResponse;
type DisassembleResponse = disassemble::DisassembleResponse;
type ResolveSourceBreakpointsResponse = breakpoints::ResolveSourceBreakpointsResponse;
type ResolveSourceLocationsResponse = breakpoints::ResolveSourceLocationsResponse;
type StepResult = core_ops::StepResult;

type WriteMemory8Request = WriteMemoryRequest<u8>;
type WriteMemory16Request = WriteMemoryRequest<u16>;
type WriteMemory32Request = WriteMemoryRequest<u32>;
type WriteMemory64Request = WriteMemoryRequest<u64>;

type CoreStatusResponse = RpcResult<WireCoreStatus>;
type CoreInfoResponse = RpcResult<WireCoreInformation>;
type ResetAndHaltResponse = RpcResult<WireCoreInformation>;
type CoreMetadataResponse = RpcResult<WireCoreMetadata>;
type CoreReadRegistersResponse = RpcResult<Vec<WireRegisterReadResult>>;
type CoreDumpResponse = RpcResult<WireCoreDump>;
type HandleSemihostingResponse =
    RpcResult<crate::rpc::functions::core_ops::HandleSemihostingResult>;
type CoreSetHwBpsResponse = RpcResult<Vec<Result<(), RpcError>>>;

endpoints! {
    list = ENDPOINT_LIST;
    | EndpointTy                | RequestTy               | ResponseTy              | Path               |
    | ----------                | ---------               | ----------              | ----               |
    | ListProbesEndpoint        | ()                      | ListProbesResponse      | "probe/list"       |
    | SelectProbeEndpoint       | SelectProbeRequest      | SelectProbeResponse     | "probe/select"     |
    | AttachEndpoint            | AttachRequest           | AttachResponse          | "probe/attach"     |

    | ResumeAllCoresEndpoint    | ResumeAllCoresRequest   | NoResponse              | "resume"           |
    | BuildEndpoint             | BuildRequest            | BuildResponse           | "flash/build"      |
    | FlashEndpoint             | FlashRequest            | NoResponse              | "flash/flash"      |
    | EraseEndpoint             | EraseRequest            | NoResponse              | "flash/erase"      |
    | VerifyEndpoint            | VerifyRequest           | VerifyResponse          | "flash/verify"     |
    | MonitorEndpoint           | MonitorRequest          | MonitorResponse         | "monitor"          |

    | TakeStackTraceEndpoint     | TakeStackTraceRequest     | TakeStackTraceResponse     | "stack_trace"              |
    | TakeRichStackTraceEndpoint | TakeRichStackTraceRequest | TakeRichStackTraceResponse | "stack_trace/rich"         |
    | ScopesEndpoint             | ScopesRequest             | ScopesResponse             | "stack_trace/scopes"       |
    | VariablesEndpoint          | VariablesRequest          | VariablesResponse          | "stack_trace/variables"    |
    | EvaluateEndpoint           | EvaluateRequest           | EvaluateResponse           | "stack_trace/evaluate"     |
    | SetVariableEndpoint        | SetVariableRequest        | SetVariableResponse        | "stack_trace/set_variable" |

    | LoadDebugInfoEndpoint            | LoadDebugInfoRequest            | LoadDebugInfoResponse            | "debug_state/load_debug_info"            |
    | ResolveSourceBreakpointsEndpoint | ResolveSourceBreakpointsRequest | ResolveSourceBreakpointsResponse | "debug_state/resolve_source_breakpoints" |
    | ResolveSourceLocationsEndpoint   | ResolveSourceLocationsRequest   | ResolveSourceLocationsResponse   | "debug_state/resolve_source_locations"   |
    | ClearCoreDebugStateEndpoint      | ClearCoreDebugStateRequest      | NoResponse                       | "debug_state/clear_core"                 |
    | LoadSvdEndpoint                  | LoadSvdRequest                  | LoadSvdResponse                  | "debug_state/load_svd"                   |

    | CreateRttClientEndpoint      | CreateRttClientRequest | CreateRttClientResponse | "create_rtt"              |
    | RttDownEndpoint              | RttDownRequest         | NoResponse              | "rtt/down"                |
    | GetRttChannelsEndpoint       | RttChannelRequest      | RttChannelsResponse     | "rtt/channels"            |
    | PollRttUpEndpoint            | PollRttUpRequest       | PollRttUpResponse       | "rtt/poll_up"             |
    | CleanUpRttEndpoint           | RttChannelRequest      | NoResponse              | "rtt/clean_up"            |
    | ClearRttControlBlockEndpoint | RttChannelRequest      | NoResponse              | "rtt/clear_control_block" |

    | ListTestsEndpoint         | ListTestsRequest        | ListTestsResponse       | "tests/list"       |
    | RunTestEndpoint           | RunTestRequest          | RunTestResponse         | "tests/run"        |
    | TestKickoffEndpoint       | TestKickoffRequest      | TestKickoffResponse     | "tests/kickoff"    |

    | CreateTempFileEndpoint    | ()                      | CreateFileResponse      | "temp_file/new"    |
    | TempFileDataEndpoint      | AppendFileRequest       | NoResponse              | "temp_file/append" |

    | ListChipFamiliesEndpoint  | ()                      | ListFamiliesResponse    | "chips/list"       |
    | ChipInfoEndpoint          | ChipInfoRequest         | ChipInfoResponse        | "chips/info"       |
    | LoadChipFamilyEndpoint    | LoadChipFamilyRequest   | NoResponse              | "chips/load"       |

    | TargetMetadataEndpoint    | TargetMetadataRequest   | TargetMetadataResponse  | "target/metadata"  |
    | TargetInfoEndpoint        | TargetInfoRequest       | NoResponse              | "info"             |
    | ResetCoreEndpoint         | ResetCoreRequest        | NoResponse              | "reset"            |
    | ResetCoreAndHaltEndpoint  | ResetCoreAndHaltRequest | ResetAndHaltResponse    | "reset_and_halt"   |

    | CoreStatusEndpoint           | CoreAccessRequest        | CoreStatusResponse         | "core/status"             |
    | CoreHaltEndpoint             | CoreHaltRequest          | CoreInfoResponse           | "core/halt"               |
    | CoreRunEndpoint              | CoreAccessRequest        | NoResponse                 | "core/run"                |
    | CoreStepEndpoint             | StepRequest              | StepResult                 | "core/step"               |
    | CoreWriteRegEndpoint         | CoreWriteRegRequest      | NoResponse                 | "core/write_reg"          |
    | CoreSetHwBpsEndpoint         | CoreBreakpointsRequest   | CoreSetHwBpsResponse       | "core/set_hw_bps"         |
    | CoreClearHwBpsEndpoint       | CoreBreakpointsRequest   | NoResponse                 | "core/clear_hw_bps"       |
    | CoreEnableVcEndpoint         | CoreVectorCatchRequest   | NoResponse                 | "core/enable_vc"          |
    | CoreMetadataEndpoint         | CoreAccessRequest        | CoreMetadataResponse       | "core/metadata"           |
    | CoreReadRegistersEndpoint    | CoreReadRegistersRequest | CoreReadRegistersResponse  | "core/read_registers"     |
    | CoreDumpEndpoint             | CoreDumpRequest          | CoreDumpResponse           | "core/dump"               |
    | HandleSemihostingEndpoint    | HandleSemihostingRequest | HandleSemihostingResponse  | "core/handle_semihosting" |
    | DisassembleEndpoint          | DisassembleRequest       | DisassembleResponse        | "core/disassemble"        |

    | ReadMemory8Endpoint       | ReadMemoryRequest       | ReadMemory8Response     | "memory/read8"     |
    | ReadMemory16Endpoint      | ReadMemoryRequest       | ReadMemory16Response    | "memory/read16"    |
    | ReadMemory32Endpoint      | ReadMemoryRequest       | ReadMemory32Response    | "memory/read32"    |
    | ReadMemory64Endpoint      | ReadMemoryRequest       | ReadMemory64Response    | "memory/read64"    |
    | ReadBytesEndpoint         | ReadBytesRequest        | ReadBytesResponse       | "memory/read_bytes" |

    | WriteMemory8Endpoint      | WriteMemory8Request     | NoResponse              | "memory/write8"    |
    | WriteMemory16Endpoint     | WriteMemory16Request    | NoResponse              | "memory/write16"   |
    | WriteMemory32Endpoint     | WriteMemory32Request    | NoResponse              | "memory/write32"   |
    | WriteMemory64Endpoint     | WriteMemory64Request    | NoResponse              | "memory/write64"   |
}

topics! {
    list = TOPICS_IN_LIST;
    direction = TopicDirection::ToServer;
    | TopicTy     | MessageTy     | Path     |
    | -------     | ---------     | ----     |
    | CancelTopic | ()            | "cancel" |
}

topics! {
    list = TOPICS_OUT_LIST;
    direction = TopicDirection::ToClient;
    | TopicTy             | MessageTy        | Path             | Cfg |
    | -------             | ---------        | ----             | --- |
    | TargetInfoDataTopic | InfoEvent        | "info/data"      |     |
    | ProgressEventTopic  | ProgressEvent    | "flash/progress" |     |
    | RttTopic            | RttEvent         | "rtt"            |     |
    | SemihostingTopic    | SemihostingEvent | "semihosting"    |     |
}

postcard_rpc::define_dispatch! {
    app: RpcApp;
    spawn_fn: spawn_fn;
    tx_impl: WireTxImpl;
    spawn_impl: TokioSpawner;
    context: RpcContext;

    endpoints: {
        list: ENDPOINT_LIST;

        | EndpointTy                | kind      | handler           |
        | ----------                | ----      | -------           |
        | ListProbesEndpoint        | blocking  | list_probes       |
        | SelectProbeEndpoint       | async     | select_probe      |
        | AttachEndpoint            | async     | attach            |

        | ResumeAllCoresEndpoint           | async | resume_all_cores           |
        | CreateRttClientEndpoint          | async | create_rtt_client          |
        | TakeStackTraceEndpoint           | async | take_stack_trace           |
        | TakeRichStackTraceEndpoint       | async | take_rich_stack_trace      |
        | LoadDebugInfoEndpoint            | async | load_debug_info            |
        | ResolveSourceBreakpointsEndpoint | async | resolve_source_breakpoints |
        | ResolveSourceLocationsEndpoint   | async | resolve_source_locations   |
        | ScopesEndpoint                   | async | debug_scopes               |
        | VariablesEndpoint                | async | debug_variables            |
        | ClearCoreDebugStateEndpoint      | async | clear_core_debug_state     |
        | LoadSvdEndpoint                  | async | debug_load_svd             |
        | EvaluateEndpoint                 | async | debug_evaluate             |
        | SetVariableEndpoint              | async | debug_set_variable         |
        | DisassembleEndpoint              | async | disassemble_handler        |
        | BuildEndpoint                    | async | build                      |
        | FlashEndpoint                    | async | flash                      |
        | EraseEndpoint                    | async | erase                      |
        | VerifyEndpoint                   | async | verify                     |
        | MonitorEndpoint                  | spawn | monitor                    |
        | RttDownEndpoint                  | async | write_rtt_down             |
        | GetRttChannelsEndpoint           | async | get_rtt_channels           |
        | PollRttUpEndpoint                | async | poll_rtt_up                |
        | CleanUpRttEndpoint               | async | clean_up_rtt               |
        | ClearRttControlBlockEndpoint     | async | clear_rtt_control_block    |

        | ListTestsEndpoint                | spawn | list_tests                 |
        | RunTestEndpoint                  | spawn | run_test                   |
        | TestKickoffEndpoint              | async | test_kickoff               |

        | CreateTempFileEndpoint           | async | create_temp_file           |
        | TempFileDataEndpoint             | async | append_temp_file           |

        | ListChipFamiliesEndpoint         | async | list_families              |
        | ChipInfoEndpoint                 | async | chip_info                  |
        | LoadChipFamilyEndpoint           | async | load_chip_family           |

        | TargetMetadataEndpoint           | async | target_metadata            |
        | TargetInfoEndpoint               | async | target_info                |
        | ResetCoreEndpoint                | async | reset                      |
        | ResetCoreAndHaltEndpoint         | async | reset_and_halt             |

        | CoreStatusEndpoint               | async | core_status                |
        | CoreHaltEndpoint                 | async | core_halt                  |
        | CoreRunEndpoint                  | async | core_run                   |
        | CoreStepEndpoint                 | async | core_step                  |
        | CoreWriteRegEndpoint             | async | core_write_reg             |
        | CoreSetHwBpsEndpoint             | async | core_set_hw_bps            |
        | CoreClearHwBpsEndpoint           | async | core_clear_hw_bps          |
        | CoreEnableVcEndpoint             | async | core_enable_vc             |
        | CoreMetadataEndpoint             | async | core_metadata              |
        | CoreReadRegistersEndpoint        | async | core_read_registers        |
        | CoreDumpEndpoint                 | async | core_dump                  |
        | HandleSemihostingEndpoint        | async | core_handle_semihosting    |

        | ReadMemory8Endpoint              | async | read_memory                |
        | ReadMemory16Endpoint             | async | read_memory                |
        | ReadMemory32Endpoint             | async | read_memory                |
        | ReadMemory64Endpoint             | async | read_memory                |
        | ReadBytesEndpoint                | async | read_bytes                 |

        | WriteMemory8Endpoint             | async | write_memory               |
        | WriteMemory16Endpoint            | async | write_memory               |
        | WriteMemory32Endpoint            | async | write_memory               |
        | WriteMemory64Endpoint            | async | write_memory               |
    };
    topics_in: {
        list: TOPICS_IN_LIST;

        | TopicTy                   | kind      | handler                       |
        | ----------                | ----      | -------                       |
        | CancelTopic               | async     | cancel_handler                |
    };
    topics_out: {
        list: TOPICS_OUT_LIST;
    };
}

pub type WireTxImpl = WireTx<Sender<Vec<u8>>>;
pub type WireRxImpl = WireRx<Receiver<Result<Vec<u8>, WireRxErrorKind>>>;

type ServerImpl = Server<WireTxImpl, WireRxImpl, Box<[u8]>, RpcApp>;
type TxChannel = Sender<Result<Vec<u8>, WireRxErrorKind>>;
type RxChannel = Receiver<Vec<u8>>;

impl RpcApp {
    pub fn create_server(
        depth: usize,
        probe_access: ProbeAccess,
    ) -> (ServerImpl, TxChannel, RxChannel) {
        Self::create_server_with_lister(depth, Arc::new(LimitedLister::new(probe_access)))
    }

    /// Like [`RpcApp::create_server`] but with a custom probe lister. Used by
    /// tests that drive the in-process RPC server with a `FakeProbe`.
    pub fn create_server_with_lister(
        depth: usize,
        lister: Arc<dyn ProbeLister + Send + Sync>,
    ) -> (ServerImpl, TxChannel, RxChannel) {
        let client_to_server = channel::<Result<Vec<u8>, WireRxErrorKind>>(depth);
        let server_to_client = channel::<Vec<u8>>(depth);

        let client_to_server_rx = WireRx::new(client_to_server.1);
        let server_to_client_tx = WireTx::new(server_to_client.0);

        let mut dispatcher = RpcApp::new(RpcContext::with_lister(lister), TokioSpawner);
        let vkk = dispatcher.min_key_len();
        dispatcher
            .context
            .set_sender(PostcardSender::new(server_to_client_tx.clone(), vkk));

        (
            Server::new(
                server_to_client_tx,
                client_to_server_rx,
                vec![0u8; 1024 * 1024].into_boxed_slice(), // 1MB buffer
                dispatcher,
                vkk,
            ),
            client_to_server.0,
            server_to_client.1,
        )
    }
}
