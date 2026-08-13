//! A client for the probe-rs RPC interface.
//!
//! Programs that talk to a `probe-rs serve` server depend on this crate.
//! Enable the `remote` feature for websocket, SSH, and unix socket transport.

use postcard_rpc::{
    Topic,
    header::{VarSeq, VarSeqKind},
    host_client::{HostClient, HostClientConfig, HostErr, IoClosed, Subscription},
    standard_icd::WireError,
};
use postcard_schema::Schema;
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    sync::{Mutex, Notify},
    time::timeout,
};

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

mod upload_cache;

use upload_cache::UploadCache;
pub use upload_cache::{ContentHash, ResolvedUpload};

use probe_rs_rpc::breakpoints::{
    BreakpointResolution, ResolveSourceBreakpointsRequest, ResolveSourceLocationsRequest,
    SourceBreakpointLocation, WireSourceLocation,
};
use probe_rs_rpc::chip::{ChipData, ChipFamily, ChipInfoRequest, LoadChipFamilyRequest};
use probe_rs_rpc::core_ops::{
    CoreAccessRequest, CoreBreakpointsRequest, CoreDumpRequest, CoreHaltRequest,
    CoreReadRegistersRequest, CoreVectorCatchRequest, CoreWriteRegRequest,
    HandleSemihostingRequest, HandleSemihostingResult, StepRequest, StepResponse, WireCoreDump,
    WireCoreInformation, WireCoreMetadata, WireCoreStatus, WireRegisterId, WireRegisterReadResult,
    WireRegisterValue, WireSteppingMode, WireVectorCatchCondition,
};
use probe_rs_rpc::cores::{CoresRequest, CoresStatusMap, HaltCoresRequest};
use probe_rs_rpc::debug_vars::{
    ClearCoreDebugStateRequest, EvaluateRequest, LoadSvdRequest, ScopesRequest, SetVariableRequest,
    VariablesRequest, WireEvaluateResponse, WireScope, WireSetVariableResponse, WireVariable,
};
use probe_rs_rpc::disassemble::{DisassembleRequest, WireDisassembledInstruction};
use probe_rs_rpc::file::{AppendFileRequest, TempFile};
use probe_rs_rpc::flash::{
    BootInfo, BootRequest, BuildRequest, BuildResult, DownloadOptions, EraseAllRequest,
    EraseRangeRequest, FlashRequest, LoadRegionRequest, NewFlashLoaderRequest, ProgressEvent,
    VerifyRequest, VerifyResult,
};
use probe_rs_rpc::format::FormatOptions;
use probe_rs_rpc::info::{
    InfoEvent, TargetInfoRequest, TargetMetadataRequest, WireSessionTargetMetadata,
};
use probe_rs_rpc::memory::{ReadBytesRequest, ReadMemoryRequest, WriteMemoryRequest};
use probe_rs_rpc::monitor::{
    MonitorExitReason, MonitorMode, MonitorOptions, MonitorRequest, RttEvent, SemihostingEvent,
};
use probe_rs_rpc::probe::{
    AttachRequest, AttachResult, DebugProbeEntry, DebugProbeSelector, SelectProbeRequest,
    SelectProbeResult,
};
use probe_rs_rpc::reset::{ResetCoreAndHaltRequest, ResetCoreRequest};
use probe_rs_rpc::rtt_client::{
    CreateRttClientRequest, PollRttUpRequest, RttChannelRequest, RttChannels, RttClientData,
    RttDownRequest, RttPollResult, ScanRegion,
};
use probe_rs_rpc::rtt_config::RttChannelConfig;
use probe_rs_rpc::semihosting_options::SemihostingOptions;
use probe_rs_rpc::stack_trace::{
    LoadDebugInfoRequest, RichStackTraces, StackTraces, TakeRichStackTraceRequest,
    TakeStackTraceRequest,
};
use probe_rs_rpc::test::{
    ListTestsRequest, RunTestRequest, Test, TestKickoffRequest, TestResult, Tests,
};
use probe_rs_rpc::transport::memory::{PostcardReceiver, PostcardSender, WireRx, WireTx};
use probe_rs_rpc::{
    AttachEndpoint, BootEndpoint, BuildEndpoint, ChipInfoEndpoint, CleanUpRttEndpoint,
    ClearCoreDebugStateEndpoint, ClearRttControlBlockEndpoint, CoreClearHwBpsEndpoint,
    CoreDumpEndpoint, CoreEnableVcEndpoint, CoreHaltEndpoint, CoreMetadataEndpoint,
    CoreReadRegistersEndpoint, CoreRunEndpoint, CoreSetHwBpsEndpoint, CoreStatusEndpoint,
    CoreStepEndpoint, CoreWriteRegEndpoint, CoresStatusEndpoint, CreateRttClientEndpoint,
    CreateTempFileEndpoint, DisassembleEndpoint, EraseAllEndpoint, EraseRangeEndpoint,
    EvaluateEndpoint, FlashEndpoint, GetRttChannelsEndpoint, HaltCoresEndpoint,
    HandleSemihostingEndpoint, ListChipFamiliesEndpoint, ListProbesEndpoint, ListTestsEndpoint,
    LoadChipFamilyEndpoint, LoadDebugInfoEndpoint, LoadRegionEndpoint, LoadSvdEndpoint,
    MonitorEndpoint, NewFlashLoaderEndpoint, PollRttUpEndpoint, ProgressEventTopic,
    ReadBytesEndpoint, ReadMemory8Endpoint, ReadMemory16Endpoint, ReadMemory32Endpoint,
    ReadMemory64Endpoint, ResetCoreAndHaltEndpoint, ResetCoreEndpoint,
    ResolveSourceBreakpointsEndpoint, ResolveSourceLocationsEndpoint, ResumeCoresEndpoint,
    RpcError, RpcResult, RttDownEndpoint, RttTopic, RunTestEndpoint, ScopesEndpoint,
    SelectProbeEndpoint, SemihostingTopic, SetVariableEndpoint, TakeRichStackTraceEndpoint,
    TakeStackTraceEndpoint, TargetInfoDataTopic, TargetInfoEndpoint, TargetMetadataEndpoint,
    TempFileDataEndpoint, TestKickoffEndpoint, TokioSpawner, VariablesEndpoint, VerifyEndpoint,
    WriteMemory8Endpoint, WriteMemory16Endpoint, WriteMemory32Endpoint, WriteMemory64Endpoint,
};
use probe_rs_rpc::{FlashLoader, Key, RttClient, Session};

/// Host and optional authentication token identifying a remote probe-rs RPC
/// server. `None` selects a local, in-process server.
pub type RemoteParams = Option<(String, Option<String>)>;

#[derive(Debug, docsplay::Display, thiserror::Error)]
pub enum TransportError {
    /// Wire error: {0}
    Wire(WireError),
    /// Bad response
    BadResponse,
    /// Postcard error: {0}
    Postcard(#[source] postcard::Error),
    /// Connection closed
    Closed,
    /// {0}
    Message(String),
}

#[derive(Debug, docsplay::Display, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// The connection to the server failed.
    #[display("{0}")]
    Transport(#[from] TransportError),
    /// The server does not know this endpoint. The client and the server
    /// versions may differ.
    UnknownEndpoint,
    /// The server refused the request.
    #[display("{0}")]
    Remote(RpcError),
    /// Failed to parse server URI.
    InvalidRemoteHost,
    /// Could not start ssh.
    SshSpawn(#[source] std::io::Error),
    /// Failed to read {0}.
    FileRead(PathBuf, #[source] std::io::Error),
}

fn from_host_err(e: HostErr<WireError>) -> ClientError {
    match e {
        HostErr::Wire(WireError::UnknownKey) => ClientError::UnknownEndpoint,
        HostErr::Wire(w) => ClientError::Transport(TransportError::Wire(w)),
        HostErr::BadResponse => ClientError::Transport(TransportError::BadResponse),
        HostErr::Postcard(e) => ClientError::Transport(TransportError::Postcard(e)),
        HostErr::Closed => ClientError::Transport(TransportError::Closed),
    }
}

fn from_io_closed(_: IoClosed) -> ClientError {
    ClientError::Transport(TransportError::Closed)
}

#[cfg(feature = "remote")]
mod ssh;

#[cfg(feature = "remote")]
async fn rpc_client_from_websocket<S>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
    challenge: &str,
    token: Option<&str>,
) -> Result<RpcClient, TransportError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use futures_util::StreamExt as _;
    use probe_rs_rpc::transport::websocket::{WebsocketRx, WebsocketTx};
    use sha2::{Digest, Sha512};
    use tokio_tungstenite::tungstenite::Message;
    use tokio_util::bytes::Bytes;

    let mut hasher = Sha512::new();
    hasher.update(challenge.as_bytes());
    hasher.update(token.unwrap_or_default().as_bytes());
    let challenge_response = hasher.finalize().to_vec();

    let (tx, rx) = ws_stream.split();

    let tx = WebsocketTx::new(tx);
    tx.send(challenge_response).await.map_err(|err| {
        TransportError::Message(format!("Failed to send challenge response: {err:?}"))
    })?;

    Ok(RpcClient::new_from_wire(
        tx,
        WebsocketRx::new(rx.map(|message| {
            message.map(|message| match message {
                Message::Binary(binary) => binary,
                _ => Bytes::new(),
            })
        })),
    ))
}

/// Connect to a `probe-rs serve` server.
///
/// `host` selects the transport by its prefix:
///
/// - `ws://` or `wss://`: a websocket.
/// - `ssh://`, followed by `[user@]destination[:port]`: a websocket that runs
///   over `ssh -W`. The port defaults to 3000, and names the port of the
///   server on the loopback interface of the remote host, not the ssh port.
///   Every other ssh setting comes from the ssh configuration file of the
///   user.
/// - `socket://`, followed by a path: a unix socket. Unix only.
#[cfg(feature = "remote")]
pub async fn connect(
    host: &str,
    token: Option<&str>,
    user_agent: &str,
) -> Result<RpcClient, ClientError> {
    use http::Uri;
    use rustls::ClientConfig;
    use std::str::FromStr;
    use tokio_tungstenite::{connect_async_tls_with_config, tungstenite::ClientRequestBuilder};

    #[cfg(unix)]
    if let Some(path) = host.strip_prefix("socket://") {
        tracing::debug!("Socket path detected, will connect via Unix socket.");

        return connect_unix(path).await;
    }

    if let Some(ssh_host) = host.strip_prefix("ssh://") {
        return ssh::connect(ssh_host, token, user_agent).await;
    }

    let uri =
        Uri::from_str(&format!("{host}/worker")).map_err(|_| ClientError::InvalidRemoteHost)?;

    // We could check the host address for localhost and then set the `is_localhost` option, but
    // there are setups where the user uses port forwarding and the file actually needs to be
    // uploaded for correct behavior. Therefore, this check is not performed.

    let req = ClientRequestBuilder::new(uri).with_header("User-Agent", user_agent);

    // TODO: implement something more secure
    let rustls_connector = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(tls::NoCertificateVerification::new(
            rustls::crypto::ring::default_provider(),
        )))
        .with_no_client_auth();

    let (ws_stream, resp) = connect_async_tls_with_config(
        req,
        None,
        false,
        Some(tokio_tungstenite::Connector::Rustls(Arc::new(
            rustls_connector,
        ))),
    )
    .await
    .map_err(|_| TransportError::Message(format!("Failed to connect to {host}")))?;

    // Respond to the challenge
    let challenge = resp
        .headers()
        .get("Probe-Rs-Challenge")
        .ok_or(TransportError::Message("No challenge header".into()))?
        .to_str()
        .map_err(|_| TransportError::Message("Failed to parse challenge header".into()))?;

    rpc_client_from_websocket(ws_stream, challenge, token)
        .await
        .map_err(ClientError::Transport)
}

#[cfg(all(feature = "remote", unix))]
pub async fn connect_unix(path: &str) -> Result<RpcClient, ClientError> {
    use probe_rs_rpc::transport::unix::{UnixStreamRx, UnixStreamTx};
    use tokio::net::UnixStream;

    let stream = UnixStream::connect(path).await.map_err(|err| {
        TransportError::Message(format!("Failed to connect to Unix socket: {err:?}"))
    })?;

    let (reader, writer) = stream.into_split();

    let tx = UnixStreamTx::new(writer);
    let rx = UnixStreamRx::new(reader);

    Ok(RpcClient::new_from_wire(tx, rx))
}

#[cfg(feature = "remote")]
mod tls {
    use rustls::DigitallySignedStruct;
    use rustls::client::danger::HandshakeSignatureValid;
    use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

    #[derive(Debug)]
    pub struct NoCertificateVerification(CryptoProvider);

    impl NoCertificateVerification {
        pub fn new(provider: CryptoProvider) -> Self {
            Self(provider)
        }
    }

    impl rustls::client::danger::ServerCertVerifier for NoCertificateVerification {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp: &[u8],
            _now: UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            verify_tls12_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            verify_tls13_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }
}

/// Websocket-backed connection to a remote probe-rs server.
#[derive(Clone)]
pub struct RpcClient {
    client: HostClient<WireError>,
    upload_cache: Arc<Mutex<UploadCache>>,
    is_localhost: bool,
}

impl Drop for RpcClient {
    fn drop(&mut self) {
        if Arc::strong_count(&self.upload_cache) == 1 {
            // Dropping the last client
            self.client.close();
        }
    }
}

impl RpcClient {
    pub fn new_from_wire(
        tx: impl PostcardSender + Send + Sync + 'static,
        rx: impl PostcardReceiver + Send + 'static,
    ) -> RpcClient {
        Self {
            client: HostClient::<WireError>::new_with_wire_and_config(
                WireTx::new(tx),
                WireRx::new(rx),
                TokioSpawner,
                &HostClientConfig {
                    seq_kind: VarSeqKind::Seq2,
                    err_uri_path: "error",
                    outgoing_depth: 1,
                    subscriber_timeout_if_full: Duration::from_secs(1),
                },
            ),
            upload_cache: Arc::new(Mutex::new(UploadCache::default())),
            is_localhost: false,
        }
    }

    pub fn is_local_session(&self) -> bool {
        self.is_localhost
    }

    pub fn new_local_from_wire(
        tx: impl PostcardSender + Send + Sync + 'static,
        rx: impl PostcardReceiver + Send + 'static,
    ) -> RpcClient {
        let mut this = Self::new_from_wire(tx, rx);
        this.is_localhost = true;
        this
    }

    async fn send<E, T>(&self, req: &E::Request) -> Result<T, ClientError>
    where
        E: postcard_rpc::Endpoint<Response = T>,
        E::Request: Serialize + Schema,
        E::Response: DeserializeOwned + Schema,
    {
        self.client.send_resp::<E>(req).await.map_err(from_host_err)
    }

    async fn send_resp<E, T>(&self, req: &E::Request) -> Result<T, ClientError>
    where
        E: postcard_rpc::Endpoint<Response = RpcResult<T>>,
        E::Request: Serialize + Schema,
        E::Response: DeserializeOwned + Schema,
    {
        self.send::<E, RpcResult<T>>(req)
            .await?
            .map_err(ClientError::Remote)
    }

    pub async fn publish<T: Topic>(&self, message: &T::Message) -> Result<(), ClientError>
    where
        T::Message: Serialize,
    {
        self.client
            .publish::<T>(VarSeq::Seq2(0), message)
            .await
            .map_err(from_io_closed)
    }

    async fn send_and_read_stream<E, T, R>(
        &self,
        req: &E::Request,
        on_msg: impl AsyncFnMut(T::Message),
    ) -> Result<R, ClientError>
    where
        E: postcard_rpc::Endpoint<Response = RpcResult<R>>,
        E::Request: Serialize + Schema,
        E::Response: DeserializeOwned + Schema,
        T: MultiTopic,
    {
        let mut stream = T::subscribe(&self.client, 64).await?;
        let notify = Arc::new(Notify::new());
        let req_fut = async {
            let res = self.send_resp::<E, R>(req).await;
            notify.notify_one();
            res
        };

        let (_, res) = tokio::join! {
            stream.stream(on_msg, notify.clone()),
            req_fut,
        };
        res
    }

    /// Resolve a local file to the path the RPC server should use, along with
    /// its content identity.
    ///
    /// Reads and hashes the file once, reuses a prior remote upload when the
    /// canonical path and content hash match, and uploads only on cache miss.
    /// Failed uploads never update the cache. The file is hashed even for a
    /// local session, where the returned hash is the caller's only way to tell
    /// whether the contents changed since a previous resolve.
    pub async fn resolve_upload(&self, src_path: &Path) -> Result<ResolvedUpload, ClientError> {
        let src_path = src_path
            .canonicalize()
            .unwrap_or_else(|_| src_path.to_path_buf());

        let data = tokio::fs::read(&src_path)
            .await
            .map_err(|e| ClientError::FileRead(src_path.clone(), e))?;
        let content_hash = ContentHash::from_bytes(&data);

        if self.is_localhost {
            return Ok(ResolvedUpload {
                canonical_path: src_path.clone(),
                content_hash,
                remote_path: src_path,
            });
        }

        if let Some(remote_path) = self
            .upload_cache
            .lock()
            .await
            .lookup(&src_path, content_hash)
        {
            tracing::debug!("Reusing cached upload for {}", src_path.display());
            return Ok(ResolvedUpload {
                canonical_path: src_path,
                content_hash,
                remote_path,
            });
        }

        let remote_path = self
            .upload_bytes(&src_path, &data)
            .await
            .map_err(|e| TransportError::Message(format!("Failed to upload file: {e}")))?;

        let mut cache = self.upload_cache.lock().await;
        if let Some(existing) = cache.lookup(&src_path, content_hash) {
            return Ok(ResolvedUpload {
                canonical_path: src_path,
                content_hash,
                remote_path: existing,
            });
        }
        cache.insert(src_path.clone(), content_hash, remote_path.clone());

        Ok(ResolvedUpload {
            canonical_path: src_path,
            content_hash,
            remote_path,
        })
    }

    /// Make a local file available to the RPC server, returning the path the
    /// server should read.
    ///
    /// A prior upload is reused only when the path *and* its contents match, so
    /// rebuilding a binary between calls uploads the new bytes rather than
    /// silently reusing the stale copy. Unlike [`Self::resolve_upload`], a local
    /// session never reads the file, since the server reads it in place.
    pub async fn upload_file(&self, src_path: &Path) -> Result<PathBuf, ClientError> {
        if self.is_localhost {
            return Ok(src_path
                .canonicalize()
                .unwrap_or_else(|_| src_path.to_path_buf()));
        }

        Ok(self.resolve_upload(src_path).await?.remote_path)
    }

    async fn upload_bytes(&self, src_path: &Path, data: &[u8]) -> Result<PathBuf, ClientError> {
        tracing::debug!("Uploading {} ({} bytes)", src_path.display(), data.len());

        let TempFile { key, path } = self.send_resp::<CreateTempFileEndpoint, _>(&()).await?;

        for chunk in data.chunks(1024 * 512) {
            self.send_resp::<TempFileDataEndpoint, _>(&AppendFileRequest {
                data: chunk.into(),
                key,
            })
            .await?;
        }

        tracing::debug!("Uploaded file to {path}");
        Ok(PathBuf::from(path))
    }

    pub async fn attach_probe(&self, request: AttachRequest) -> Result<AttachResult, ClientError> {
        self.send_resp::<AttachEndpoint, _>(&request).await
    }

    pub async fn list_probes(&self) -> Result<Vec<DebugProbeEntry>, ClientError> {
        self.send_resp::<ListProbesEndpoint, _>(&()).await
    }

    pub async fn select_probe(
        &self,
        selector: Option<DebugProbeSelector>,
    ) -> Result<SelectProbeResult, ClientError> {
        self.send_resp::<SelectProbeEndpoint, _>(&SelectProbeRequest { probe: selector })
            .await
    }

    pub async fn info(
        &self,
        request: TargetInfoRequest,
        on_msg: impl AsyncFnMut(InfoEvent),
    ) -> Result<(), ClientError> {
        self.send_and_read_stream::<TargetInfoEndpoint, TargetInfoDataTopic, _>(&request, on_msg)
            .await
    }

    pub async fn load_chip_family(&self, families_yaml: String) -> Result<(), ClientError> {
        self.send_resp::<LoadChipFamilyEndpoint, _>(&LoadChipFamilyRequest { families_yaml })
            .await
    }

    pub async fn list_chip_families(&self) -> Result<Vec<ChipFamily>, ClientError> {
        self.send_resp::<ListChipFamiliesEndpoint, _>(&()).await
    }

    pub async fn chip_info(&self, name: &str) -> Result<ChipData, ClientError> {
        self.send_resp::<ChipInfoEndpoint, _>(&ChipInfoRequest { name: name.into() })
            .await
    }
}

#[derive(Clone)]
pub struct SessionInterface {
    sessid: Key<Session>,
    client: RpcClient,
}

impl SessionInterface {
    pub fn new(client: RpcClient, sessid: Key<Session>) -> Self {
        Self { sessid, client }
    }

    pub fn client(&self) -> RpcClient {
        self.client.clone()
    }

    pub async fn target_metadata(&self) -> Result<WireSessionTargetMetadata, ClientError> {
        self.client
            .send_resp::<TargetMetadataEndpoint, _>(&TargetMetadataRequest {
                sessid: self.sessid,
            })
            .await
    }

    /// The server-side [`Key`] identifying the attached [`Session`].
    ///
    /// Exposed so that alternate backends (e.g. the DAP server's RPC
    /// backend) can reuse the same session identifier when building their
    /// own client types.
    pub fn session_key(&self) -> Key<Session> {
        self.sessid
    }

    pub fn core(&self, core: usize) -> CoreInterface {
        CoreInterface {
            sessid: self.sessid,
            core: core as u32,
            client: self.client.clone(),
        }
    }

    pub async fn resume_all_cores(&self) -> Result<(), ClientError> {
        self.resume_cores(None).await.map(|_| ())
    }

    /// Halt selected cores and return the status of each active core.
    ///
    /// When `cores` is `None`, every session core is considered. Disabled cores
    /// are omitted from the returned map.
    pub async fn halt_cores(
        &self,
        cores: Option<Vec<u32>>,
        timeout: Duration,
    ) -> Result<CoresStatusMap, ClientError> {
        self.client
            .send_resp::<HaltCoresEndpoint, _>(&HaltCoresRequest {
                sessid: self.sessid,
                cores,
                timeout,
            })
            .await
    }

    /// Resume selected cores and return the status of each active core.
    ///
    /// When `cores` is `None`, every session core is considered. Disabled cores
    /// are omitted from the returned map.
    pub async fn resume_cores(
        &self,
        cores: Option<Vec<u32>>,
    ) -> Result<CoresStatusMap, ClientError> {
        self.client
            .send_resp::<ResumeCoresEndpoint, _>(&CoresRequest {
                sessid: self.sessid,
                cores,
            })
            .await
    }

    /// Read the status of selected cores.
    ///
    /// When `cores` is `None`, every session core is considered. Disabled cores
    /// are omitted from the returned map.
    pub async fn cores_status(
        &self,
        cores: Option<Vec<u32>>,
    ) -> Result<CoresStatusMap, ClientError> {
        self.client
            .send_resp::<CoresStatusEndpoint, _>(&CoresRequest {
                sessid: self.sessid,
                cores,
            })
            .await
    }

    /// Prepares the core to execute the loaded image.
    ///
    /// When `resume` is true, all cores are started afterward. When false, the
    /// cores stay halted after prepare.
    ///
    /// If the image runs from RAM, the target does not get a reset. If the image
    /// runs from flash, the target gets a reset.
    pub async fn boot(&self, boot_info: BootInfo, core_id: usize) -> Result<(), ClientError> {
        self.boot_with_resume(boot_info, core_id, true).await
    }

    /// Prepares the core to execute the loaded image and leaves cores halted.
    pub async fn prepare_boot(
        &self,
        boot_info: BootInfo,
        core_id: usize,
    ) -> Result<(), ClientError> {
        self.boot_with_resume(boot_info, core_id, false).await
    }

    async fn boot_with_resume(
        &self,
        boot_info: BootInfo,
        core_id: usize,
        resume: bool,
    ) -> Result<(), ClientError> {
        self.client
            .send_resp::<BootEndpoint, _>(&BootRequest {
                sessid: self.sessid,
                boot_info,
                core_id: core_id as u32,
                resume,
            })
            .await
    }

    pub async fn new_flash_loader(
        &self,
        read_flasher_rtt: bool,
    ) -> Result<Key<FlashLoader>, ClientError> {
        self.client
            .send_resp::<NewFlashLoaderEndpoint, _>(&NewFlashLoaderRequest {
                sessid: self.sessid,
                read_flasher_rtt,
            })
            .await
    }

    pub async fn load_region(
        &self,
        loader: Key<FlashLoader>,
        address: u64,
        data: Vec<u8>,
    ) -> Result<(), ClientError> {
        self.client
            .send_resp::<LoadRegionEndpoint, _>(&LoadRegionRequest {
                sessid: self.sessid,
                loader,
                address,
                data,
            })
            .await
    }

    pub async fn build_flash_loader(
        &self,
        path: PathBuf,
        format: FormatOptions,
        image_target: Option<String>,
        read_flasher_rtt: bool,
    ) -> Result<BuildResult, ClientError> {
        let upload = self.client.resolve_upload(&path).await?;
        self.build_flash_loader_resolved(&upload, format, image_target, read_flasher_rtt)
            .await
    }

    pub async fn build_flash_loader_resolved(
        &self,
        upload: &ResolvedUpload,
        mut format: FormatOptions,
        image_target: Option<String>,
        read_flasher_rtt: bool,
    ) -> Result<BuildResult, ClientError> {
        let path = upload.server_path().to_path_buf();

        if let Some(ref mut idf_bootloader) = format.idf_options.idf_bootloader {
            *idf_bootloader = self
                .client
                .upload_file(idf_bootloader.as_ref())
                .await?
                .display()
                .to_string();
        }

        if let Some(ref mut idf_partition_table) = format.idf_options.idf_partition_table {
            *idf_partition_table = self
                .client
                .upload_file(idf_partition_table.as_ref())
                .await?
                .display()
                .to_string();
        }

        self.client
            .send_resp::<BuildEndpoint, _>(&BuildRequest {
                sessid: self.sessid,
                path: path.display().to_string(),
                format,
                image_target,
                read_flasher_rtt,
            })
            .await
    }

    pub async fn flash(
        &self,
        options: DownloadOptions,
        loader: Key<FlashLoader>,
        rtt_client: Option<Key<RttClient>>,
        on_msg: impl AsyncFnMut(ProgressEvent),
    ) -> Result<(), ClientError> {
        self.client
            .send_and_read_stream::<FlashEndpoint, ProgressEventTopic, _>(
                &FlashRequest {
                    sessid: self.sessid,
                    loader,
                    options,
                    rtt_client,
                },
                on_msg,
            )
            .await
    }

    pub async fn erase_all(
        &self,
        read_flasher_rtt: bool,
        on_msg: impl AsyncFnMut(ProgressEvent),
    ) -> Result<(), ClientError> {
        self.client
            .send_and_read_stream::<EraseAllEndpoint, ProgressEventTopic, _>(
                &EraseAllRequest {
                    sessid: self.sessid,
                    read_flasher_rtt,
                },
                on_msg,
            )
            .await
    }

    pub async fn erase_range(
        &self,
        address: u64,
        length: u64,
        restore: bool,
        read_flasher_rtt: bool,
        on_msg: impl AsyncFnMut(ProgressEvent),
    ) -> Result<(), ClientError> {
        self.client
            .send_and_read_stream::<EraseRangeEndpoint, ProgressEventTopic, _>(
                &EraseRangeRequest {
                    sessid: self.sessid,
                    address,
                    length,
                    restore,
                    read_flasher_rtt,
                },
                on_msg,
            )
            .await
    }

    pub async fn monitor(
        &self,
        mode: MonitorMode,
        options: MonitorOptions,
        on_msg: impl AsyncFnMut(MonitorEvent),
    ) -> Result<MonitorExitReason, ClientError> {
        self.client
            .send_and_read_stream::<MonitorEndpoint, MonitorEvent, _>(
                &MonitorRequest {
                    sessid: self.sessid,
                    mode,
                    options,
                },
                on_msg,
            )
            .await
    }

    /// Write to an RTT down channel, returning how many bytes the target
    /// accepted until finish or Timeout. Timeout = 0 -> single attempt
    pub async fn send_to_rtt(
        &self,
        rtt_client: Key<RttClient>,
        channel: u32,
        data: Vec<u8>,
        timeout_ms: u32,
    ) -> Result<u32, ClientError> {
        self.client
            .send_resp::<RttDownEndpoint, _>(&RttDownRequest {
                sessid: self.sessid,
                rtt_client,
                channel,
                data,
                timeout_ms,
            })
            .await
    }

    pub async fn list_tests(
        &self,
        boot_info: BootInfo,
        rtt_client: Option<Key<RttClient>>,
        semihosting_options: SemihostingOptions,
        on_msg: impl AsyncFnMut(MonitorEvent),
    ) -> Result<Tests, ClientError> {
        self.client
            .send_and_read_stream::<ListTestsEndpoint, MonitorEvent, _>(
                &ListTestsRequest {
                    sessid: self.sessid,
                    boot_info,
                    rtt_client,
                    semihosting_options,
                },
                on_msg,
            )
            .await
    }

    pub async fn run_test(
        &self,
        test: Test,
        rtt_client: Option<Key<RttClient>>,
        semihosting_options: SemihostingOptions,
        on_msg: impl AsyncFnMut(MonitorEvent),
    ) -> Result<TestResult, ClientError> {
        self.client
            .send_and_read_stream::<RunTestEndpoint, MonitorEvent, _>(
                &RunTestRequest {
                    sessid: self.sessid,
                    test,
                    rtt_client,
                    semihosting_options,
                },
                on_msg,
            )
            .await
    }

    pub async fn create_rtt_client(
        &self,
        scan_regions: ScanRegion,
        config: Vec<RttChannelConfig>,
        default_config: RttChannelConfig,
    ) -> Result<RttClientData, ClientError> {
        self.client
            .send_resp::<CreateRttClientEndpoint, _>(&CreateRttClientRequest {
                sessid: self.sessid,
                scan_regions,
                config,
                default_config,
            })
            .await
    }

    /// Attach the server-side RTT client and return its up/down channel
    /// metadata.
    pub async fn get_rtt_channels(
        &self,
        rtt_client: Key<RttClient>,
    ) -> Result<RttChannels, ClientError> {
        self.client
            .send_resp::<GetRttChannelsEndpoint, _>(&RttChannelRequest {
                sessid: self.sessid,
                rtt_client,
            })
            .await
    }

    /// Poll multiple up channels on the server-side RTT client in one
    /// request, returning the newly-available bytes (and any per-channel
    /// error) for each.
    pub async fn poll_rtt_up(
        &self,
        rtt_client: Key<RttClient>,
        channels: Vec<u32>,
    ) -> Result<Vec<RttPollResult>, ClientError> {
        self.client
            .send_resp::<PollRttUpEndpoint, _>(&PollRttUpRequest {
                sessid: self.sessid,
                rtt_client,
                channels,
            })
            .await
    }

    /// Restore the original mode of every up channel on the server-side
    /// RTT client.
    pub async fn clean_up_rtt(&self, rtt_client: Key<RttClient>) -> Result<(), ClientError> {
        self.client
            .send_resp::<CleanUpRttEndpoint, _>(&RttChannelRequest {
                sessid: self.sessid,
                rtt_client,
            })
            .await
    }

    /// Wipe a stale RTT control block from target memory, before a reset or
    /// reflash.
    pub async fn clear_rtt_control_block(
        &self,
        rtt_client: Key<RttClient>,
    ) -> Result<(), ClientError> {
        self.client
            .send_resp::<ClearRttControlBlockEndpoint, _>(&RttChannelRequest {
                sessid: self.sessid,
                rtt_client,
            })
            .await
    }

    /// Path-based stack trace for generic CLI callers. Uploads and parses
    /// DWARF from `path` on each request; does not use session
    /// `ServerDebugState`. DAP stack refresh uses
    /// [`Self::take_rich_stack_trace`] instead.
    pub async fn stack_trace(
        &self,
        path: PathBuf,
        stack_frame_limit: u32,
    ) -> Result<StackTraces, ClientError> {
        let path = self.client.upload_file(&path).await?;

        self.client
            .send_resp::<TakeStackTraceEndpoint, _>(&TakeStackTraceRequest {
                sessid: self.sessid,
                path: path.display().to_string(),
                stack_frame_limit,
            })
            .await
    }

    /// Eagerly load and cache the server-side `DebugInfo` for this session
    /// from `path`, so server-side consumers (e.g. `disassemble`) can resolve
    /// source locations before the first halt. Mirrors the local backend,
    /// which loads `DebugInfo` at session start. Repeated calls replace the
    /// server copy and invalidate DWARF-derived server state.
    pub async fn load_debug_info(&self, path: PathBuf) -> Result<(), ClientError> {
        let upload = self.client.resolve_upload(&path).await?;
        self.load_debug_info_resolved(&upload).await
    }

    /// Publish server-side DWARF from a prior [`ResolvedUpload`].
    pub async fn load_debug_info_resolved(
        &self,
        upload: &ResolvedUpload,
    ) -> Result<(), ClientError> {
        self.client
            .send_resp::<LoadDebugInfoEndpoint, _>(&LoadDebugInfoRequest {
                sessid: self.sessid,
                path: upload.server_path().display().to_string(),
            })
            .await
    }

    /// Resolve a local path to a single upload identity for reuse across a
    /// restart transaction (validate, flash, publish debug info).
    pub async fn resolve_upload(&self, path: &Path) -> Result<ResolvedUpload, ClientError> {
        self.client.resolve_upload(path).await
    }

    /// Resolve source file/line requests against the server-owned debug info.
    pub async fn resolve_source_breakpoints(
        &self,
        locations: Vec<SourceBreakpointLocation>,
    ) -> Result<Vec<BreakpointResolution>, ClientError> {
        self.client
            .send_resp::<ResolveSourceBreakpointsEndpoint, _>(&ResolveSourceBreakpointsRequest {
                sessid: self.sessid,
                locations,
            })
            .await
    }

    /// Resolve instruction addresses to source metadata using server DWARF.
    pub async fn resolve_source_locations(
        &self,
        addresses: Vec<u64>,
    ) -> Result<Vec<Option<WireSourceLocation>>, ClientError> {
        self.client
            .send_resp::<ResolveSourceLocationsEndpoint, _>(&ResolveSourceLocationsRequest {
                sessid: self.sessid,
                addresses,
            })
            .await
    }

    /// Replace the server-side per-core SVD state, or clear it when `path` is
    /// `None`. The old cache is cleared before upload/parse so a failed reload
    /// cannot leave stale peripheral metadata visible.
    pub async fn load_svd(&self, core: u32, path: Option<PathBuf>) -> Result<(), ClientError> {
        self.client
            .send_resp::<LoadSvdEndpoint, _>(&LoadSvdRequest {
                sessid: self.sessid,
                core,
                path: None,
            })
            .await?;

        let Some(path) = path else {
            return Ok(());
        };
        let path = self.client.upload_file(&path).await?;

        self.client
            .send_resp::<LoadSvdEndpoint, _>(&LoadSvdRequest {
                sessid: self.sessid,
                core,
                path: Some(path.display().to_string()),
            })
            .await
    }

    /// Fetch a rich stack trace (per-frame register state + display metadata,
    /// no local variables) for the requested core(s). Requires server-side
    /// debug state from [`Self::load_debug_info`]; does not upload or parse a
    /// binary path.
    pub async fn take_rich_stack_trace(
        &self,
        core: Option<u32>,
        stack_frame_limit: u32,
    ) -> Result<RichStackTraces, ClientError> {
        self.client
            .send_resp::<TakeRichStackTraceEndpoint, _>(&TakeRichStackTraceRequest {
                sessid: self.sessid,
                core,
                stack_frame_limit,
            })
            .await
    }

    /// Resolve DAP scopes for a frame on the server against the server-owned
    /// `VariableCache`.
    pub async fn scopes(&self, core: u32, frame_id: u32) -> Result<Vec<WireScope>, ClientError> {
        self.client
            .send_resp::<ScopesEndpoint, _>(&ScopesRequest {
                sessid: self.sessid,
                core,
                frame_id,
            })
            .await
    }

    /// Resolve DAP variables for a `variables_reference` on the server,
    /// expanding lazily server-side.
    pub async fn variables(
        &self,
        core: u32,
        variables_reference: u32,
        filter: Option<String>,
    ) -> Result<Vec<WireVariable>, ClientError> {
        self.client
            .send_resp::<VariablesEndpoint, _>(&VariablesRequest {
                sessid: self.sessid,
                core,
                variables_reference,
                filter,
            })
            .await
    }

    /// Clear a core's server-owned stack and variable caches while preserving
    /// binary-independent state such as SVD variables. Called before target
    /// execution changes so stale frame and variable handles are not served.
    pub async fn clear_core_debug_state(&self, core: u32) -> Result<(), ClientError> {
        self.client
            .send_resp::<ClearCoreDebugStateEndpoint, _>(&ClearCoreDebugStateRequest {
                sessid: self.sessid,
                core,
            })
            .await
    }

    /// Evaluate a watch/hover expression server-side against the cached
    /// `VariableCache` for the given frame, expanding lazily server-side.
    pub async fn evaluate(
        &self,
        core: u32,
        frame_id: Option<u32>,
        expression: String,
    ) -> Result<WireEvaluateResponse, ClientError> {
        self.client
            .send_resp::<EvaluateEndpoint, _>(&EvaluateRequest {
                sessid: self.sessid,
                core,
                frame_id,
                expression,
            })
            .await
    }

    /// Full `SteppingMode::step` (over/into/out/instruction) run server-side
    /// against the cached `DebugInfo` and the live `Core`. Returns the new
    /// status, program counter, and any `WarnAndContinue` message.
    pub async fn debug_step(
        &self,
        core: u32,
        mode: WireSteppingMode,
    ) -> Result<StepResponse, ClientError> {
        self.client
            .send_resp::<CoreStepEndpoint, _>(&StepRequest {
                sessid: self.sessid,
                core,
                mode,
            })
            .await
    }

    /// Set a local/static variable's value server-side (the `VariableCache`
    /// lives server-side). Returns the response fields for the DAP
    /// `setVariable` response body.
    pub async fn set_variable(
        &self,
        core: u32,
        parent_key: i64,
        name: String,
        value: String,
    ) -> Result<WireSetVariableResponse, ClientError> {
        self.client
            .send_resp::<SetVariableEndpoint, _>(&SetVariableRequest {
                sessid: self.sessid,
                core,
                parent_key,
                name,
                value,
            })
            .await
    }

    pub async fn disassemble(
        &self,
        core: u32,
        memory_reference: u64,
        byte_offset: i64,
        instruction_offset: i64,
        instruction_count: i64,
    ) -> Result<Vec<WireDisassembledInstruction>, ClientError> {
        self.client
            .send_resp::<DisassembleEndpoint, _>(&DisassembleRequest {
                sessid: self.sessid,
                core,
                memory_reference,
                byte_offset,
                instruction_offset,
                instruction_count,
            })
            .await
    }

    pub async fn verify(
        &self,
        loader: Key<FlashLoader>,
        on_msg: impl AsyncFnMut(ProgressEvent),
    ) -> Result<VerifyResult, ClientError> {
        self.client
            .send_and_read_stream::<VerifyEndpoint, ProgressEventTopic, _>(
                &VerifyRequest {
                    sessid: self.sessid,
                    loader,
                },
                on_msg,
            )
            .await
    }
}

#[derive(Clone)]
pub struct CoreInterface {
    sessid: Key<Session>,
    core: u32,
    client: RpcClient,
}

impl CoreInterface {
    /// Create a client for a specific core on an attached session.
    ///
    /// Used by the RPC-backed DAP backend, which needs to synthesize a core
    /// client on every access.
    pub fn new_for_backend(client: RpcClient, sessid: Key<Session>, core: u32) -> Self {
        Self {
            sessid,
            core,
            client,
        }
    }
}

impl CoreInterface {
    pub async fn read_memory_8(&self, address: u64, count: usize) -> Result<Vec<u8>, ClientError> {
        self.client
            .send_resp::<ReadMemory8Endpoint, _>(&ReadMemoryRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                count: count as u32,
            })
            .await
    }

    /// Lossy bulk byte read: returns as many bytes as are readable starting
    /// at `address`, stopping at the first unreadable region.
    pub async fn read_bytes(&self, address: u64, count: usize) -> Result<Vec<u8>, ClientError> {
        self.client
            .send_resp::<ReadBytesEndpoint, _>(&ReadBytesRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                count: count as u64,
            })
            .await
    }
    pub async fn read_memory_16(
        &self,
        address: u64,
        count: usize,
    ) -> Result<Vec<u16>, ClientError> {
        self.client
            .send_resp::<ReadMemory16Endpoint, _>(&ReadMemoryRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                count: count as u32,
            })
            .await
    }
    pub async fn read_memory_32(
        &self,
        address: u64,
        count: usize,
    ) -> Result<Vec<u32>, ClientError> {
        self.client
            .send_resp::<ReadMemory32Endpoint, _>(&ReadMemoryRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                count: count as u32,
            })
            .await
    }
    pub async fn read_memory_64(
        &self,
        address: u64,
        count: usize,
    ) -> Result<Vec<u64>, ClientError> {
        self.client
            .send_resp::<ReadMemory64Endpoint, _>(&ReadMemoryRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                count: count as u32,
            })
            .await
    }

    pub async fn write_memory_8(&self, address: u64, data: Vec<u8>) -> Result<(), ClientError> {
        self.client
            .send_resp::<WriteMemory8Endpoint, _>(&WriteMemoryRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                data,
            })
            .await
    }
    pub async fn write_memory_16(&self, address: u64, data: Vec<u16>) -> Result<(), ClientError> {
        self.client
            .send_resp::<WriteMemory16Endpoint, _>(&WriteMemoryRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                data,
            })
            .await
    }
    pub async fn write_memory_32(&self, address: u64, data: Vec<u32>) -> Result<(), ClientError> {
        self.client
            .send_resp::<WriteMemory32Endpoint, _>(&WriteMemoryRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                data,
            })
            .await
    }
    pub async fn write_memory_64(&self, address: u64, data: Vec<u64>) -> Result<(), ClientError> {
        self.client
            .send_resp::<WriteMemory64Endpoint, _>(&WriteMemoryRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                data,
            })
            .await
    }

    pub async fn reset(&self) -> Result<(), ClientError> {
        self.client
            .send_resp::<ResetCoreEndpoint, _>(&ResetCoreRequest {
                sessid: self.sessid,
                core: self.core,
            })
            .await
    }

    pub async fn reset_and_halt(
        &self,
        timeout: Duration,
    ) -> Result<WireCoreInformation, ClientError> {
        self.client
            .send_resp::<ResetCoreAndHaltEndpoint, _>(&ResetCoreAndHaltRequest {
                sessid: self.sessid,
                core: self.core,
                timeout,
            })
            .await
    }

    fn access_request(&self) -> CoreAccessRequest {
        CoreAccessRequest {
            sessid: self.sessid,
            core: self.core,
        }
    }

    pub async fn status(&self) -> Result<WireCoreStatus, ClientError> {
        self.client
            .send_resp::<CoreStatusEndpoint, _>(&self.access_request())
            .await
    }

    pub async fn halt(&self, timeout: Duration) -> Result<WireCoreInformation, ClientError> {
        self.client
            .send_resp::<CoreHaltEndpoint, _>(&CoreHaltRequest {
                sessid: self.sessid,
                core: self.core,
                timeout,
            })
            .await
    }

    pub async fn run(&self) -> Result<(), ClientError> {
        self.client
            .send_resp::<CoreRunEndpoint, _>(&self.access_request())
            .await
    }

    /// Read a single register. Thin wrapper over [`Self::read_registers`]
    /// that turns the per-register failure back into an error.
    pub async fn read_core_reg(
        &self,
        id: WireRegisterId,
    ) -> Result<WireRegisterValue, ClientError> {
        let mut results = self.read_registers(vec![id]).await?;
        match results.pop() {
            Some(result) => result.result.map_err(ClientError::Remote),
            None => Err(ClientError::Transport(TransportError::Message(format!(
                "Server returned no result for register {}",
                id.0
            )))),
        }
    }

    pub async fn write_core_reg(
        &self,
        id: WireRegisterId,
        value: WireRegisterValue,
    ) -> Result<(), ClientError> {
        self.client
            .send_resp::<CoreWriteRegEndpoint, _>(&CoreWriteRegRequest {
                sessid: self.sessid,
                core: self.core,
                id,
                value,
            })
            .await
    }

    /// Set a single hardware breakpoint. Thin wrapper over
    /// [`Self::set_hw_breakpoints`] that turns the per-address failure back
    /// into an error.
    pub async fn set_hw_breakpoint(&self, address: u64) -> Result<(), ClientError> {
        let mut results = self.set_hw_breakpoints(vec![address]).await?;
        match results.pop() {
            Some(result) => result.map_err(ClientError::Remote),
            None => Err(ClientError::Transport(TransportError::Message(format!(
                "Server returned no result for breakpoint {address:#x}"
            )))),
        }
    }

    pub async fn set_hw_breakpoints(
        &self,
        addresses: Vec<u64>,
    ) -> Result<Vec<Result<(), RpcError>>, ClientError> {
        self.client
            .send_resp::<CoreSetHwBpsEndpoint, _>(&CoreBreakpointsRequest {
                sessid: self.sessid,
                core: self.core,
                addresses,
            })
            .await
    }

    pub async fn clear_hw_breakpoints(&self, addresses: Vec<u64>) -> Result<(), ClientError> {
        self.client
            .send_resp::<CoreClearHwBpsEndpoint, _>(&CoreBreakpointsRequest {
                sessid: self.sessid,
                core: self.core,
                addresses,
            })
            .await
    }

    pub async fn enable_vector_catch(
        &self,
        condition: WireVectorCatchCondition,
    ) -> Result<(), ClientError> {
        self.client
            .send_resp::<CoreEnableVcEndpoint, _>(&CoreVectorCatchRequest {
                sessid: self.sessid,
                core: self.core,
                condition,
            })
            .await
    }

    pub async fn metadata(&self) -> Result<WireCoreMetadata, ClientError> {
        self.client
            .send_resp::<CoreMetadataEndpoint, _>(&self.access_request())
            .await
    }

    /// Bulk-read a set of registers.
    ///
    /// Per-register failures are reported in place, in the same order as
    /// `ids`. Callers that need strict "all-or-nothing" semantics can inspect
    /// the returned slots themselves.
    pub async fn read_registers(
        &self,
        ids: Vec<WireRegisterId>,
    ) -> Result<Vec<WireRegisterReadResult>, ClientError> {
        self.client
            .send_resp::<CoreReadRegistersEndpoint, _>(&CoreReadRegistersRequest {
                sessid: self.sessid,
                core: self.core,
                ids,
            })
            .await
    }

    /// Dump the core (registers + the supplied memory ranges) server-side and
    /// return the wire fields so the caller can reconstruct a `CoreDump`.
    pub async fn dump_core(
        &self,
        ranges: Vec<std::ops::Range<u64>>,
    ) -> Result<WireCoreDump, ClientError> {
        self.client
            .send_resp::<CoreDumpEndpoint, _>(&CoreDumpRequest {
                sessid: self.sessid,
                core: self.core,
                ranges,
            })
            .await
    }

    /// Handle a semihosting halt server-side: the server performs the file I/O
    /// next to the target and returns the resulting core status plus the UI
    /// events the client must replay (RTT window open, console/RTT output).
    pub async fn handle_semihosting(&self) -> Result<HandleSemihostingResult, ClientError> {
        self.client
            .send_resp::<HandleSemihostingEndpoint, _>(&HandleSemihostingRequest {
                sessid: self.sessid,
                core: self.core,
            })
            .await
    }

    /// Kick off a single embedded-test case server-side: run until the
    /// `GetCommandLine` semihosting call, write `run_addr {address}` as the
    /// command line, then resume. Used by the DAP REPL `test run` command.
    pub async fn kickoff_test(&self, address: u64) -> Result<(), ClientError> {
        self.client
            .send_resp::<TestKickoffEndpoint, _>(&TestKickoffRequest {
                sessid: self.sessid,
                core: self.core,
                address,
            })
            .await
    }
}

pub(crate) trait MultiTopic {
    type Message;
    type Subscription: MultiSubscription<Message = Self::Message>;

    async fn subscribe<E>(
        client: &HostClient<E>,
        depth: usize,
    ) -> Result<Self::Subscription, ClientError>
    where
        E: DeserializeOwned + Schema;
}

impl<T> MultiTopic for T
where
    T: Topic,
    T::Message: DeserializeOwned,
{
    type Message = T::Message;
    type Subscription = Subscription<T::Message>;

    async fn subscribe<E>(
        client: &HostClient<E>,
        depth: usize,
    ) -> Result<Self::Subscription, ClientError>
    where
        E: DeserializeOwned + Schema,
    {
        client
            .subscribe_exclusive::<Self>(depth)
            .await
            .map_err(|error| {
                ClientError::Transport(TransportError::Message(format!(
                    "Failed to subscribe to '{}': {error:?}",
                    T::PATH,
                )))
            })
    }
}

pub(crate) trait MultiSubscription {
    type Message;

    async fn next(&mut self) -> Option<Self::Message>;

    /// Listen to the given stream until either:
    ///
    /// * The stream closes, returning a "closed" notification
    /// * The `stopper` notification is fired, at which point we will continue processing
    ///   messages until there is a time of 100ms between messages, at which point we will
    ///   return.
    ///
    /// The latter case is intended to cover cases where there could still be enqueued messages
    /// waiting to be processed.
    async fn stream(
        &mut self,
        mut on_msg: impl AsyncFnMut(Self::Message),
        stopper: Arc<Notify>,
    ) -> Result<(), ClientError> {
        let listen_fut = async {
            while let Some(message) = self.next().await {
                on_msg(message).await;
            }
        };

        tokio::select! {
            _ = listen_fut => {
                tracing::warn!("Failed to read topic");
                Ok(())
            }
            _ = stopper.notified() => {
                tracing::info!("Received stop");

                // We've received the stop event, now receive any pending messages.
                loop {
                    match timeout(Duration::from_millis(100), self.next()).await {
                        Ok(Some(m)) => on_msg(m).await,
                        Ok(None) | Err(_) => return Ok(()),
                    }
                }
            }
        }
    }
}

pub enum MonitorEvent {
    Rtt(RttEvent),
    Semihosting(SemihostingEvent),
}

impl MultiTopic for MonitorEvent {
    type Message = Self;
    type Subscription = MonitorSubscription;

    async fn subscribe<E>(
        client: &HostClient<E>,
        depth: usize,
    ) -> Result<Self::Subscription, ClientError>
    where
        E: DeserializeOwned + Schema,
    {
        // TODO: remove MonitorEvent from the RPC interface, split this subscribe into two:
        // one for RTT, one for semihosting, then introduce a MultiSubscription impl for them
        let rtt = RttTopic::subscribe(client, depth).await?;
        let semihosting = SemihostingTopic::subscribe(client, depth).await?;
        Ok(MonitorSubscription { rtt, semihosting })
    }
}

pub(crate) struct MonitorSubscription {
    rtt: <RttTopic as MultiTopic>::Subscription,
    semihosting: <SemihostingTopic as MultiTopic>::Subscription,
}
impl MultiSubscription for MonitorSubscription {
    type Message = MonitorEvent;

    async fn next(&mut self) -> Option<Self::Message> {
        tokio::select! {
            message = self.rtt.recv() => message.map(MonitorEvent::Rtt),
            message = self.semihosting.recv() => message.map(MonitorEvent::Semihosting),
        }
    }
}

impl<T> MultiSubscription for Subscription<T>
where
    T: DeserializeOwned,
{
    type Message = T;

    async fn next(&mut self) -> Option<Self::Message> {
        self.recv().await
    }
}
