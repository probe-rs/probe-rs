use std::{any::Any, ops::DerefMut};
use std::{convert::Infallible, future::Future};

use crate::rpc::functions::file::{
    append_temp_file, create_temp_file, AppendFileRequest, CreateFileResponse,
};
use crate::rpc::functions::probe::{select_probe, SelectProbeRequest, SelectProbeResponse};
use crate::rpc::transport::memory::{WireRx, WireTx};
use crate::{
    rpc::{
        functions::{
            chip::{
                chip_info, list_families, load_chip_family, ChipInfoRequest, ChipInfoResponse,
                ListFamiliesResponse, LoadChipFamilyRequest,
            },
            flash::{flash, FlashRequest, FlashResponse, ProgressEvent},
            info::{target_info, InfoEvent, TargetInfoRequest},
            memory::{read_memory, write_memory, ReadMemoryRequest, WriteMemoryRequest},
            monitor::{monitor, MonitorEvent, MonitorRequest},
            probe::{
                attach, list_probes, AttachRequest, AttachResponse, ListProbesRequest,
                ListProbesResponse,
            },
            reset::{reset, ResetCoreRequest},
            resume::{resume_all_cores, ResumeAllCoresRequest},
            rtt_client::{create_rtt_client, CreateRttClientRequest, CreateRttClientResponse},
            stack_trace::{take_stack_trace, TakeStackTraceRequest, TakeStackTraceResponse},
            test::{
                list_tests, run_test, ListTestsRequest, ListTestsResponse, RunTestRequest,
                RunTestResponse,
            },
        },
        Key, SessionState,
    },
    util::common_options::OperationError,
};

use anyhow::anyhow;
use postcard_rpc::header::{VarHeader, VarSeq};
use postcard_rpc::server::{
    Dispatch, Sender as PostcardSender, Server, SpawnContext, WireRxErrorKind, WireTxErrorKind,
};
use postcard_rpc::{endpoints, host_client, server, topics, Topic, TopicDirection};
use postcard_schema::Schema;
use probe_rs::{probe::list::Lister, Session};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{channel, Receiver, Sender};
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
        Self(e.to_string())
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
    pub async fn publish<T>(&self, seq_no: VarSeq, msg: &T::Message) -> anyhow::Result<()>
    where
        T: ?Sized,
        T: Topic,
        T::Message: Serialize + Schema,
    {
        anyhow::ensure!(!self.token.is_cancelled(), "RPC call cancelled");

        self.sender
            .publish::<T>(seq_no, msg)
            .await
            .map_err(|e| anyhow!("{:?}", e))
    }

    pub fn publish_blocking<T>(&self, seq_no: VarSeq, msg: T::Message) -> anyhow::Result<()>
    where
        T: Topic,
        T::Message: Serialize + Schema + Send + Sync + Sized + 'static,
    {
        let handle = tokio::runtime::Handle::current();
        let this = self.clone();
        handle.block_on(async move {
            tokio::spawn(async move { this.publish::<T>(seq_no, &msg).await })
                .await
                .unwrap()
        })
    }

    fn dry_run(&self, sessid: Key<Session>) -> bool {
        self.state.dry_run(sessid)
    }

    fn session_blocking(&self, sessid: Key<Session>) -> impl DerefMut<Target = Session> {
        self.state.session_blocking(sessid)
    }

    pub fn object_mut_blocking<T: Any + Send>(
        &self,
        key: Key<T>,
    ) -> impl DerefMut<Target = T> + Send {
        self.state.object_mut_blocking(key)
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.token.clone()
    }
}

pub struct RpcContext {
    state: SessionState,
    token: CancellationToken,
    sender: Option<PostcardSender<WireTxImpl>>,
    local: bool,
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
    pub fn new(local: bool) -> Self {
        Self {
            state: SessionState::new(),
            token: CancellationToken::new(),
            sender: None,
            local,
        }
    }

    pub fn is_local(&self) -> bool {
        self.local
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

    pub async fn object_mut<T: Any + Send>(&self, key: Key<T>) -> impl DerefMut<Target = T> + Send {
        self.state.object_mut(key).await
    }

    pub async fn store_object<T: Any + Send>(&mut self, obj: T) -> Key<T> {
        self.state.store_object(obj).await
    }

    #[allow(unused)]
    pub fn store_object_blocking<T: Any + Send>(&mut self, obj: T) -> Key<T> {
        self.state.store_object_blocking(obj)
    }

    pub async fn set_session(&mut self, session: Session, dry_run: bool) -> Key<Session> {
        self.state.set_session(session, dry_run).await
    }

    pub async fn session(&self, sid: Key<Session>) -> impl DerefMut<Target = Session> + Send {
        self.object_mut(sid).await
    }

    pub fn lister(&self) -> Lister {
        Lister::new()
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
    | FlashEndpoint             | FlashRequest           | FlashResponse           | "flash"            |
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
        | FlashEndpoint             | async     | flash             |
        | MonitorEndpoint           | spawn     | monitor           |

        | ListTestsEndpoint         | spawn     | list_tests        |
        | RunTestEndpoint           | spawn     | run_test          |

        | CreateTempFileEndpoint    | blocking  | create_temp_file  |
        | TempFileDataEndpoint      | async     | append_temp_file  |

        | ListChipFamiliesEndpoint  | blocking  | list_families     |
        | ChipInfoEndpoint          | blocking  | chip_info         |
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
    pub fn create_server(local: bool, depth: usize) -> (ServerImpl, TxChannel, RxChannel) {
        let client_to_server = channel::<Result<Vec<u8>, WireRxErrorKind>>(depth);
        let server_to_client = channel::<Vec<u8>>(depth);

        let client_to_server_rx = WireRx::new(client_to_server.1);
        let server_to_client_tx = WireTx::new(server_to_client.0);

        let mut dispatcher = RpcApp::new(RpcContext::new(local), TokioSpawner);
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