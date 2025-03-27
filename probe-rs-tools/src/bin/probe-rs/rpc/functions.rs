use std::{any::Any, ops::DerefMut};
use std::{convert::Infallible, future::Future};

use crate::rpc::functions::file::{
    AppendFileRequest, CreateFileResponse, append_temp_file, create_temp_file,
};
use crate::rpc::functions::probe::{SelectProbeRequest, SelectProbeResponse, select_probe};
use crate::rpc::transport::memory::{WireRx, WireTx};
use crate::{
    rpc::{
        Key, SessionState,
        functions::{
            chip::{
                ChipInfoRequest, ChipInfoResponse, ListFamiliesResponse, LoadChipFamilyRequest,
                chip_info, list_families, load_chip_family,
            },
            flash::{
                BuildRequest, BuildResponse, EraseRequest, FlashRequest, ProgressEvent,
                VerifyRequest, VerifyResponse, build, erase, flash, verify,
            },
            info::{InfoEvent, TargetInfoRequest, target_info},
            memory::{ReadMemoryRequest, WriteMemoryRequest, read_memory, write_memory},
            monitor::{MonitorEvent, MonitorRequest, monitor},
            probe::{
                AttachRequest, AttachResponse, ListProbesRequest, ListProbesResponse, attach,
                list_probes,
            },
            reset::{ResetCoreRequest, reset},
            resume::{ResumeAllCoresRequest, resume_all_cores},
            rtt_client::{CreateRttClientRequest, CreateRttClientResponse, create_rtt_client},
            stack_trace::{TakeStackTraceRequest, TakeStackTraceResponse, take_stack_trace},
            test::{
                ListTestsRequest, ListTestsResponse, RunTestRequest, RunTestResponse, list_tests,
                run_test,
            },
        },
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
use probe_rs::probe::list::AllProbesLister;
use probe_rs::probe::{
    DebugProbeError, DebugProbeInfo, DebugProbeSelector, Probe, ProbeCreationError,
};
use probe_rs::{Session, probe::list::Lister};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio_util::sync::CancellationToken;

pub mod chip;
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

#[derive(Debug, Serialize, Deserialize, Schema)]
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
        Self(format!("{:?}", e))
    }
}

impl From<RpcError> for anyhow::Error {
    fn from(e: RpcError) -> Self {
        anyhow!(e.0)
    }
}

#[derive(Clone)]
pub struct RpcSpawnContext {
    state: SessionState,
    token: CancellationToken,
    sender: PostcardSender<WireTxImpl>,
}

impl RpcSpawnContext {
    fn dry_run(&self, sessid: Key<Session>) -> bool {
        self.state.dry_run(sessid)
    }

    fn session_blocking(&self, sessid: Key<Session>) -> impl DerefMut<Target = Session> + use<> {
        self.state.session_blocking(sessid)
    }

    pub fn object_mut_blocking<T: Any + Send>(
        &self,
        key: Key<T>,
    ) -> impl DerefMut<Target = T> + Send + use<T> {
        self.state.object_mut_blocking(key)
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.token.clone()
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
        let (channel_sender, mut channel_receiver) = tokio::sync::mpsc::channel::<T::Message>(256);

        let sender = self.sender.clone();
        let token = self.cancellation_token();
        let sender = async move {
            loop {
                tokio::select! {
                    biased;

                    _ = token.cancelled() => break,
                    Some(event) = channel_receiver.recv() => {
                        sender
                            .publish::<T>(VarSeq::Seq2(0), &event)
                            .await
                            .unwrap();
                    }
                }
            }
            std::mem::drop(channel_receiver);

            futures_util::future::pending().await
        };

        let ctx = self.clone();
        let blocking = tokio::task::spawn_blocking(move || task(ctx, request, channel_sender));

        tokio::select! {
            _ = sender => unreachable!(),
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
    /// Create a new lister with the default lister implementation.
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

    fn list_all(&self) -> Vec<DebugProbeInfo> {
        self.all_probes
            .list_all()
            .into_iter()
            .filter(|info| self.is_allowed(&DebugProbeSelector::from(info)))
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
    state: SessionState,
    token: CancellationToken,
    sender: Option<PostcardSender<WireTxImpl>>,
    probe_access: ProbeAccess,
}

impl SpawnContext for RpcContext {
    type SpawnCtxt = RpcSpawnContext;

    fn spawn_ctxt(&mut self) -> Self::SpawnCtxt {
        self.token = CancellationToken::new();
        RpcSpawnContext {
            state: self.state.clone(),
            token: self.token.clone(),
            sender: self.sender.clone().unwrap(),
        }
    }
}

impl RpcContext {
    pub fn new(probe_access: ProbeAccess) -> Self {
        Self {
            state: SessionState::new(),
            token: CancellationToken::new(),
            sender: None,
            probe_access,
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
            .map_err(|e| anyhow!("{:?}", e))
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

    pub fn lister(&self) -> Lister {
        Lister::with_lister(Box::new(LimitedLister::new(self.probe_access.clone())))
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
    ctx.token.cancel();
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

type ReadMemory8Response = RpcResult<Vec<u8>>;
type ReadMemory16Response = RpcResult<Vec<u16>>;
type ReadMemory32Response = RpcResult<Vec<u32>>;
type ReadMemory64Response = RpcResult<Vec<u64>>;

type WriteMemory8Request = WriteMemoryRequest<u8>;
type WriteMemory16Request = WriteMemoryRequest<u16>;
type WriteMemory32Request = WriteMemoryRequest<u32>;
type WriteMemory64Request = WriteMemoryRequest<u64>;

endpoints! {
    list = ENDPOINT_LIST;
    | EndpointTy                | RequestTy              | ResponseTy              | Path               |
    | ----------                | ---------              | ----------              | ----               |
    | ListProbesEndpoint        | ListProbesRequest      | ListProbesResponse      | "probe/list"       |
    | SelectProbeEndpoint       | SelectProbeRequest     | SelectProbeResponse     | "probe/select"     |
    | AttachEndpoint            | AttachRequest          | AttachResponse          | "probe/attach"     |

    | ResumeAllCoresEndpoint    | ResumeAllCoresRequest  | NoResponse              | "resume"           |
    | CreateRttClientEndpoint   | CreateRttClientRequest | CreateRttClientResponse | "create_rtt"       |
    | TakeStackTraceEndpoint    | TakeStackTraceRequest  | TakeStackTraceResponse  | "stack_trace"      |
    | BuildEndpoint             | BuildRequest           | BuildResponse           | "flash/build"      |
    | FlashEndpoint             | FlashRequest           | NoResponse              | "flash/flash"      |
    | EraseEndpoint             | EraseRequest           | NoResponse              | "flash/erase"      |
    | VerifyEndpoint            | VerifyRequest          | VerifyResponse          | "flash/verify"     |
    | MonitorEndpoint           | MonitorRequest         | NoResponse              | "monitor"          |

    | ListTestsEndpoint         | ListTestsRequest       | ListTestsResponse       | "tests/list"       |
    | RunTestEndpoint           | RunTestRequest         | RunTestResponse         | "tests/run"        |

    | CreateTempFileEndpoint    | ()                     | CreateFileResponse      | "temp_file/new"    |
    | TempFileDataEndpoint      | AppendFileRequest      | NoResponse              | "temp_file/append" |

    | ListChipFamiliesEndpoint  | ()                     | ListFamiliesResponse    | "chips/list"       |
    | ChipInfoEndpoint          | ChipInfoRequest        | ChipInfoResponse        | "chips/info"       |
    | LoadChipFamilyEndpoint    | LoadChipFamilyRequest  | NoResponse              | "chips/load"       |

    | TargetInfoEndpoint        | TargetInfoRequest      | NoResponse              | "info"             |
    | ResetCoreEndpoint         | ResetCoreRequest       | NoResponse              | "reset"            |

    | ReadMemory8Endpoint       | ReadMemoryRequest      | ReadMemory8Response     | "memory/read8"     |
    | ReadMemory16Endpoint      | ReadMemoryRequest      | ReadMemory16Response    | "memory/read16"    |
    | ReadMemory32Endpoint      | ReadMemoryRequest      | ReadMemory32Response    | "memory/read32"    |
    | ReadMemory64Endpoint      | ReadMemoryRequest      | ReadMemory64Response    | "memory/read64"    |

    | WriteMemory8Endpoint      | WriteMemory8Request    | NoResponse              | "memory/write8"    |
    | WriteMemory16Endpoint     | WriteMemory16Request   | NoResponse              | "memory/write16"   |
    | WriteMemory32Endpoint     | WriteMemory32Request   | NoResponse              | "memory/write32"   |
    | WriteMemory64Endpoint     | WriteMemory64Request   | NoResponse              | "memory/write64"   |
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
    | TopicTy             | MessageTy     | Path              | Cfg |
    | -------             | ---------     | ----              | --- |
    | TargetInfoDataTopic | InfoEvent     | "info/data"       |     |
    | ProgressEventTopic  | ProgressEvent | "flash/progress"  |     |
    | MonitorTopic        | MonitorEvent  | "monitor"         |     |
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

        | ResumeAllCoresEndpoint    | async     | resume_all_cores  |
        | CreateRttClientEndpoint   | async     | create_rtt_client |
        | TakeStackTraceEndpoint    | async     | take_stack_trace  |
        | BuildEndpoint             | async     | build             |
        | FlashEndpoint             | async     | flash             |
        | EraseEndpoint             | async     | erase             |
        | VerifyEndpoint            | async     | verify            |
        | MonitorEndpoint           | spawn     | monitor           |

        | ListTestsEndpoint         | spawn     | list_tests        |
        | RunTestEndpoint           | spawn     | run_test          |

        | CreateTempFileEndpoint    | async     | create_temp_file  |
        | TempFileDataEndpoint      | async     | append_temp_file  |

        | ListChipFamiliesEndpoint  | async     | list_families     |
        | ChipInfoEndpoint          | async     | chip_info         |
        | LoadChipFamilyEndpoint    | async     | load_chip_family  |

        | TargetInfoEndpoint        | async     | target_info       |
        | ResetCoreEndpoint         | async     | reset             |

        | ReadMemory8Endpoint       | async     | read_memory       |
        | ReadMemory16Endpoint      | async     | read_memory       |
        | ReadMemory32Endpoint      | async     | read_memory       |
        | ReadMemory64Endpoint      | async     | read_memory       |

        | WriteMemory8Endpoint      | async     | write_memory      |
        | WriteMemory16Endpoint     | async     | write_memory      |
        | WriteMemory32Endpoint     | async     | write_memory      |
        | WriteMemory64Endpoint     | async     | write_memory      |
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
        let client_to_server = channel::<Result<Vec<u8>, WireRxErrorKind>>(depth);
        let server_to_client = channel::<Vec<u8>>(depth);

        let client_to_server_rx = WireRx::new(client_to_server.1);
        let server_to_client_tx = WireTx::new(server_to_client.0);

        let mut dispatcher = RpcApp::new(RpcContext::new(probe_access), TokioSpawner);
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
