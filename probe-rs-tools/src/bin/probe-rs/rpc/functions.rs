use std::{any::Any, ops::DerefMut, panic::AssertUnwindSafe, sync::Arc};
use std::{collections::HashMap, convert::Infallible, future::Future};

use crate::rpc::debug_state::ServerDebugState;
use crate::rpc::functions::file::{append_temp_file, create_temp_file};
use crate::rpc::probe_broker::ProbeBroker;
use crate::rpc::{
    ConnectionState, Key, Session, SessionEntry, SessionState,
    functions::{
        breakpoints::{resolve_source_breakpoints, resolve_source_locations},
        chip::{chip_info, list_families, load_chip_family},
        core_ops::{
            core_clear_hw_bps, core_dump, core_enable_vc, core_halt, core_handle_semihosting,
            core_metadata, core_read_registers, core_run, core_set_hw_bps, core_status, core_step,
            core_write_reg,
        },
        cores::{cores_status, halt_cores, resume_cores},
        debug_vars::{
            clear_core_debug_state, evaluate as debug_evaluate, load_svd as debug_load_svd,
            scopes as debug_scopes, set_variable as debug_set_variable,
            variables as debug_variables,
        },
        disassemble::disassemble as disassemble_handler,
        flash::{
            boot, build, erase_all, erase_range, flash, load_region, new_flash_loader, verify,
        },
        info::{target_info, target_metadata},
        memory::{read_bytes, read_memory, write_memory},
        monitor::monitor,
        probe::{attach, list_probes, select_probe},
        reset::{reset, reset_and_halt},
        rtt_client::{
            clean_up_rtt, clear_rtt_control_block, create_rtt_client, get_rtt_channels,
            poll_rtt_up, write_rtt_down,
        },
        stack_trace::{load_debug_info, take_rich_stack_trace, take_stack_trace},
        test::{list_tests, run_test, test_kickoff},
    },
};
use probe_rs_rpc::transport::memory::{WireRx, WireTx};

use anyhow::anyhow;
use futures_util::FutureExt;
use postcard_rpc::Topic;
use postcard_rpc::header::{VarHeader, VarSeq};
use postcard_rpc::server::{
    Dispatch, Sender as PostcardSender, Server, SpawnContext, WireRxErrorKind,
};
use postcard_schema::Schema;
use probe_rs::config::Registry;
use probe_rs::integration::ProbeLister;
use probe_rs::probe::list::Lister;
use probe_rs::probe::list::{AllProbesLister, ProbeListItem};
use probe_rs::probe::{DebugProbeError, DebugProbeSelector, Probe, ProbeCreationError};
use serde::{Deserialize, Serialize};
use tokio::sync::{
    Mutex,
    mpsc::{Receiver, Sender, channel},
};
use tokio_util::sync::CancellationToken;

pub mod breakpoints;
pub mod chip;
pub mod core_ops;
pub mod cores;
pub mod debug_vars;
pub mod disassemble;
pub mod file;
pub mod flash;
pub mod info;
pub mod memory;
pub mod monitor;
pub mod probe;
pub mod reset;
pub mod rtt_client;
pub mod stack_trace;
pub mod test;

#[derive(Clone)]
pub struct RpcSpawnContext {
    state: ConnectionState,
    sender: PostcardSender<WireTxImpl>,
    probe_broker: Arc<ProbeBroker>,
    lister: Arc<dyn ProbeLister + Send + Sync>,
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

    fn session_blocking(
        &self,
        sessid: Key<Session>,
    ) -> impl DerefMut<Target = probe_rs::Session> + use<> {
        self.shared_session(sessid).session_blocking()
    }

    fn shared_session(&self, sessid: Key<Session>) -> SessionState<'_> {
        self.state.shared_session(sessid)
    }

    pub fn object_mut_blocking<M: crate::rpc::ObjectMarker>(
        &self,
        key: Key<M>,
    ) -> impl DerefMut<Target = M::Object> + Send + use<M> {
        self.state.object_mut_blocking(key)
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.state.token.clone()
    }

    pub(crate) fn registry_blocking(&self) -> impl DerefMut<Target = Registry> + Send + use<> {
        self.state.registry.clone().blocking_lock_owned()
    }

    pub(crate) fn probe_broker(&self) -> &Arc<ProbeBroker> {
        &self.probe_broker
    }

    pub(crate) fn lister(&self) -> Lister {
        Lister::with_lister(Box::new(ArcLister(self.lister.clone())))
    }

    pub(crate) async fn set_session(
        &self,
        session: probe_rs::Session,
        dry_run: bool,
        lease: Option<crate::rpc::probe_broker::ProbeLease>,
    ) -> Key<Session> {
        self.state.set_session(session, dry_run, lease).await
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
    probe_broker: Arc<ProbeBroker>,
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
            probe_broker: self.probe_broker.clone(),
            lister: self.lister.clone(),
        }
    }
}

impl RpcContext {
    /// Build a context with a custom probe lister, bypassing the
    /// [`ProbeAccess`] filtering applied by [`LimitedLister`]. Used by tests
    /// that drive the in-process RPC server with a `FakeProbe`.
    pub fn with_lister(
        lister: Arc<dyn ProbeLister + Send + Sync>,
        probe_broker: Arc<ProbeBroker>,
    ) -> Self {
        Self {
            state: ConnectionState::new(),
            sender: None,
            lister,
            probe_broker,
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

    pub async fn object_mut<M: crate::rpc::ObjectMarker>(
        &self,
        key: Key<M>,
    ) -> impl DerefMut<Target = M::Object> + Send + use<M> {
        self.state.object_mut(key).await
    }

    pub async fn store_object<M: crate::rpc::ObjectMarker>(&mut self, obj: M::Object) -> Key<M> {
        self.state.store_object(obj).await
    }

    pub async fn session(
        &self,
        sid: Key<Session>,
    ) -> impl DerefMut<Target = probe_rs::Session> + Send + use<> {
        let locked_cell = self.state.object_storage.lock().await.cell(sid);
        let guard = locked_cell.obj.clone().lock_owned().await;
        tokio::sync::OwnedMutexGuard::map(guard, |e: &mut (dyn Any + Send)| {
            &mut e.downcast_mut::<SessionEntry>().unwrap().session
        })
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

pub fn spawn_fn(
    sp: &probe_rs_rpc::TokioSpawner,
    fut: impl Future<Output = ()> + 'static + Send,
) -> Result<(), Infallible> {
    let panicked = sp.handler_panicked.clone();
    tokio::task::spawn(async move {
        // A spawned handler answers its own request. A panic drops it before it
        // replies, so the connection must end to release the client. Nothing
        // the handler touches outlives that connection.
        if AssertUnwindSafe(fut).catch_unwind().await.is_err() {
            panicked.cancel();
        }
    });
    Ok(())
}

pub(crate) type DebugStatesMap = Arc<Mutex<HashMap<Key<Session>, ServerDebugState>>>;

use probe_rs_rpc::*;

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
        | AttachEndpoint            | spawn     | attach            |

        | HaltCoresEndpoint                | async | halt_cores                 |
        | ResumeCoresEndpoint              | async | resume_cores               |
        | CoresStatusEndpoint              | async | cores_status               |
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
        | NewFlashLoaderEndpoint           | async | new_flash_loader           |
        | BuildEndpoint                    | async | build                      |
        | LoadRegionEndpoint               | async | load_region                |
        | FlashEndpoint                    | async | flash                      |
        | EraseAllEndpoint                 | async | erase_all                  |
        | EraseRangeEndpoint               | async | erase_range                |
        | VerifyEndpoint                   | async | verify                     |
        | BootEndpoint                     | async | boot                       |
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

/// Serves a single client connection.
pub struct RpcServer {
    server: ServerImpl,
    handler_panicked: CancellationToken,
}

impl RpcServer {
    /// Answers requests until the client disconnects, or until a request
    /// handler panics.
    ///
    /// A panicked handler leaves its request unanswered. Ending the connection
    /// tells the client that the request failed, instead of leaving it to wait
    /// for a reply that nobody will send.
    pub async fn run(mut self) {
        tokio::select! {
            _ = self.server.run() => {}
            _ = self.handler_panicked.cancelled() => {
                tracing::error!("A request handler panicked. Closing the connection.");
            }
        }
    }
}

impl RpcApp {
    pub fn create_server(
        depth: usize,
        probe_access: ProbeAccess,
        probe_broker: Arc<ProbeBroker>,
    ) -> (RpcServer, TxChannel, RxChannel) {
        Self::create_server_with_lister(
            depth,
            Arc::new(LimitedLister::new(probe_access)),
            probe_broker,
        )
    }

    /// Like [`RpcApp::create_server`] but with a custom probe lister. Used by
    /// tests that drive the in-process RPC server with a `FakeProbe`.
    pub fn create_server_with_lister(
        depth: usize,
        lister: Arc<dyn ProbeLister + Send + Sync>,
        probe_broker: Arc<ProbeBroker>,
    ) -> (RpcServer, TxChannel, RxChannel) {
        let client_to_server = channel::<Result<Vec<u8>, WireRxErrorKind>>(depth);
        let server_to_client = channel::<Vec<u8>>(depth);

        let client_to_server_rx = WireRx::new(client_to_server.1);
        let server_to_client_tx = WireTx::new(server_to_client.0);

        let spawner = TokioSpawner::default();
        let handler_panicked = spawner.handler_panicked.clone();

        let mut dispatcher = RpcApp::new(RpcContext::with_lister(lister, probe_broker), spawner);
        let vkk = dispatcher.min_key_len();
        dispatcher
            .context
            .set_sender(PostcardSender::new(server_to_client_tx.clone(), vkk));

        (
            RpcServer {
                server: Server::new(
                    server_to_client_tx,
                    client_to_server_rx,
                    vec![0u8; 1024 * 1024].into_boxed_slice(), // 1MB buffer
                    dispatcher,
                    vkk,
                ),
                handler_panicked,
            },
            client_to_server.0,
            server_to_client.1,
        )
    }
}

pub(crate) mod convert {
    use crate::util::common_options::OperationError;
    use probe_rs_rpc::{RpcError, RpcResult};

    pub(crate) fn rpc_error_anyhow(e: anyhow::Error) -> RpcError {
        format!("{e:?}").into()
    }

    pub(crate) fn rpc_error_anyhow_from<E: Into<anyhow::Error>>(e: E) -> RpcError {
        rpc_error_anyhow(e.into())
    }

    pub(crate) fn lift<T, E: Into<anyhow::Error>>(result: Result<T, E>) -> RpcResult<T> {
        result.map_err(rpc_error_anyhow_from)
    }

    pub(crate) fn rpc_error_probe_rs(e: probe_rs::Error) -> RpcError {
        rpc_error_anyhow_from(e)
    }

    pub(crate) fn rpc_error_debug(e: probe_rs_debug::DebugError) -> RpcError {
        rpc_error_anyhow_from(e)
    }

    pub(crate) fn rpc_error_flash(e: probe_rs::flashing::FlashError) -> RpcError {
        rpc_error_anyhow_from(e)
    }

    pub(crate) fn rpc_error_rtt(e: probe_rs::rtt::Error) -> RpcError {
        rpc_error_anyhow_from(e)
    }

    impl From<OperationError> for RpcError {
        fn from(e: OperationError) -> RpcError {
            rpc_error_anyhow_from(e)
        }
    }
}
