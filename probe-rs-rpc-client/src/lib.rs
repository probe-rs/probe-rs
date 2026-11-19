//! A client for the probe-rs RPC interface.
//!
//! Programs that talk to a `probe-rs serve` server depend on this crate.
//! Enable the `remote` feature for websocket, SSH, and unix socket transport.

use postcard_rpc::{
    Topic, TopicDirection,
    header::{VarSeq, VarSeqKind},
    host_client::{
        HostClient, HostClientConfig, HostErr, IoClosed, MultiSubRxError, SchemaError,
        SchemaReport, Subscription,
    },
    standard_icd::{GetAllSchemaDataTopic, GetAllSchemasEndpoint, OwnedSchemaData, WireError},
};
use postcard_schema::Schema;
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    sync::{Mutex, Notify},
    time::timeout,
};

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

mod schema;
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
use probe_rs_rpc::file::{AppendFileRequest, ServerPath, TempFile};
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
    SerialEvent, SerialWriteRequest,
};
use probe_rs_rpc::probe::{
    AttachRequest, AttachResult, DebugProbeEntry, DebugProbeSelector, SelectProbeRequest,
    SelectProbeResult,
};
use probe_rs_rpc::reset::{ResetCoreAndHaltRequest, ResetCoreRequest, ResetTargetRequest};
use probe_rs_rpc::rtt_client::{
    CreateRttClientRequest, PollRttUpRequest, RttChannelRequest, RttChannels, RttClientData,
    RttDownRequest, RttPollResult, ScanRegion,
};
use probe_rs_rpc::rtt_config::RttChannelConfig;
use probe_rs_rpc::stack_trace::{
    LoadDebugInfoRequest, RichStackTraces, StackTraces, TakeRichStackTraceRequest,
    TakeStackTraceRequest,
};
use probe_rs_rpc::test::{
    ListTestsRequest, RunTestRequest, Test, TestKickoffRequest, TestResult, Tests,
};
use probe_rs_rpc::transport::{
    Closer,
    memory::{PostcardReceiver, PostcardSender, WireRx, WireTx},
};
use probe_rs_rpc::{
    AttachEndpoint, BootEndpoint, BuildEndpoint, ChipInfoEndpoint, CleanUpRttEndpoint,
    ClearCoreDebugStateEndpoint, ClearRttControlBlockEndpoint, CoreClearHwBpsEndpoint,
    CoreDumpEndpoint, CoreEnableVcEndpoint, CoreHaltEndpoint, CoreMetadataEndpoint,
    CoreReadRegistersEndpoint, CoreRunEndpoint, CoreSetHwBpsEndpoint, CoreStatusEndpoint,
    CoreStepEndpoint, CoreWriteRegEndpoint, CoresStatusEndpoint, CreateRttClientEndpoint,
    CreateTempFileEndpoint, DisassembleEndpoint, EraseAllEndpoint, EraseRangeEndpoint,
    EvaluateEndpoint, FlashEndpoint, GetRttChannelsEndpoint, HaltCoresEndpoint,
    HandleSemihostingEndpoint, HostCallTopic, ListChipFamiliesEndpoint, ListProbesEndpoint,
    ListTestsEndpoint, LoadChipFamilyEndpoint, LoadDebugInfoEndpoint, LoadRegionEndpoint,
    LoadSvdEndpoint, MonitorEndpoint, NewFlashLoaderEndpoint, PollRttUpEndpoint,
    ProgressEventTopic, ReadBytesEndpoint, ReadMemory8Endpoint, ReadMemory16Endpoint,
    ReadMemory32Endpoint, ReadMemory64Endpoint, ResetCoreAndHaltEndpoint, ResetCoreEndpoint,
    ResetTargetEndpoint, ResolveSourceBreakpointsEndpoint, ResolveSourceLocationsEndpoint,
    ResumeCoresEndpoint, RpcError, RpcResult, RttDownEndpoint, RttTopic, RunTestEndpoint,
    ScopesEndpoint, SelectProbeEndpoint, SemihostingTopic, SerialTopic, SerialWriteEndpoint,
    SetVariableEndpoint, TakeRichStackTraceEndpoint, TakeStackTraceEndpoint, TargetInfoDataTopic,
    TargetInfoEndpoint, TargetMetadataEndpoint, TempFileDataEndpoint, TestKickoffEndpoint,
    TokioSpawner, VariablesEndpoint, VerifyEndpoint, WriteBytesEndpoint, WriteMemory8Endpoint,
    WriteMemory16Endpoint, WriteMemory32Endpoint, WriteMemory64Endpoint,
};
use probe_rs_rpc::{FlashLoader, Key, RttClient, Session};
#[cfg(feature = "remote")]
use probe_rs_rpc::{
    ServerProofEndpoint,
    server_proof::{ServerProofRequest, server_proof},
};

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
    /// The RPC schema of the server does not match this client.
    ///
    /// Use the same probe-rs version for the client and the server.
    IncompatibleServer,
    /// The client cannot verify this server, so it does not give it access to semihosting files.
    ///
    /// Use `ssh://` or a unix socket, or allow an unverified server if you trust this network.
    UnverifiedServer,
    /// Another monitor or test call of this connection already uses semihosting files.
    HostFilesInUse,
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

pub(crate) fn from_host_err(e: HostErr<WireError>) -> ClientError {
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

mod host_files;
mod semihosting_mappings;
mod semihosting_options;

pub use host_files::HostFiles;
pub use semihosting_mappings::MappingError;
pub use semihosting_options::{Mapping, SemihostingOptions};

#[cfg(feature = "remote")]
mod ssh;

#[cfg(feature = "remote")]
async fn rpc_client_from_websocket<S>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
    challenge: &str,
    token: Option<&str>,
) -> Result<RpcClient, ClientError>
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
    let closer = tx.closer();
    tx.send(challenge_response).await.map_err(|err| {
        TransportError::Message(format!("Failed to send challenge response: {err:?}"))
    })?;

    RpcClient::new_from_wire(
        tx,
        WebsocketRx::new(rx.map(|message| {
            message.map(|message| match message {
                Message::Binary(binary) => binary,
                _ => Bytes::new(),
            })
        })),
    )
    .with_closer(closer)
    .ensure_compatible()
    .await
}

/// Returns the URI of the worker endpoint of a `ws://` or `wss://` server.
#[cfg(feature = "remote")]
fn worker_uri(connection_string: &str) -> Result<http::Uri, ClientError> {
    use http::{
        Uri,
        uri::{PathAndQuery, Scheme},
    };
    use std::str::FromStr;

    let mut parts = Uri::from_str(connection_string)
        .map_err(|_| ClientError::InvalidRemoteHost)?
        .into_parts();
    // A scheme is case-insensitive, but tungstenite accepts only the lowercase form.
    let scheme = parts
        .scheme
        .as_ref()
        .map(|scheme| scheme.as_str().to_ascii_lowercase());
    let Some(scheme @ ("ws" | "wss")) = scheme.as_deref() else {
        return Err(ClientError::InvalidRemoteHost);
    };
    let path_and_query = parts.path_and_query.as_ref();
    let has_query = path_and_query.is_some_and(|path_and_query| path_and_query.query().is_some());
    let has_host = parts
        .authority
        .as_ref()
        .is_some_and(|authority| !authority.host().is_empty());
    if !has_host || has_query {
        return Err(ClientError::InvalidRemoteHost);
    }

    let prefix = path_and_query
        .map_or("", |path_and_query| path_and_query.path())
        .trim_end_matches('/');
    let worker = PathAndQuery::try_from(format!("{prefix}/worker"))
        .map_err(|_| ClientError::InvalidRemoteHost)?;
    parts.scheme = Some(Scheme::from_str(scheme).map_err(|_| ClientError::InvalidRemoteHost)?);
    parts.path_and_query = Some(worker);
    Uri::from_parts(parts).map_err(|_| ClientError::InvalidRemoteHost)
}

/// Returns `uri` without the user information, which can hold a password.
#[cfg(feature = "remote")]
fn without_userinfo(uri: &http::Uri) -> String {
    let authority = uri.authority().map_or("", |authority| authority.as_str());
    let host_and_port = authority.rsplit('@').next().unwrap_or_default();
    format!(
        "{}://{host_and_port}{}",
        uri.scheme_str().unwrap_or_default(),
        uri.path()
    )
}

/// Connect to a `probe-rs serve` server.
///
/// `connection_string` selects the transport by its prefix:
///
/// - `ws://` or `wss://`: a websocket. A path, for example the prefix of a
///   reverse proxy, comes before the `/worker` endpoint.
/// - `ssh://`, followed by `[user@]destination[:port]`: a websocket that runs
///   over `ssh -W`. The port defaults to 3000, and names the port of the
///   server on the loopback interface of the remote host, not the ssh port.
///   Every other ssh setting comes from the ssh configuration file of the
///   user.
/// - `socket://`, followed by a path: a unix socket. Unix only.
#[cfg(feature = "remote")]
pub async fn connect(
    connection_string: &str,
    token: Option<&str>,
    user_agent: &str,
) -> Result<RpcClient, ClientError> {
    use rustls::ClientConfig;
    use tokio_tungstenite::{connect_async_tls_with_config, tungstenite::ClientRequestBuilder};

    #[cfg(unix)]
    if let Some(path) = connection_string.strip_prefix("socket://") {
        tracing::debug!("Socket path detected, will connect via Unix socket.");

        return connect_unix(path).await;
    }

    if let Some(destination) = connection_string.strip_prefix("ssh://") {
        return ssh::connect(destination, token, user_agent).await;
    }

    let uri = worker_uri(connection_string)?;
    let shown_uri = without_userinfo(&uri);

    // We could check the host address for localhost and then set the `is_localhost` option, but
    // there are setups where the user uses port forwarding and the file actually needs to be
    // uploaded for correct behavior. Therefore, this check is not performed.

    let req = ClientRequestBuilder::new(uri).with_header("User-Agent", user_agent);

    // TODO: implement something more secure
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let rustls_connector = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|error| TransportError::Message(format!("Failed to configure TLS: {error}")))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(tls::NoCertificateVerification::new(
            (*provider).clone(),
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
    .map_err(|error| {
        TransportError::Message(format!("Failed to connect to {shown_uri}: {error}"))
    })?;

    // Respond to the challenge
    let challenge = resp
        .headers()
        .get("Probe-Rs-Challenge")
        .ok_or(TransportError::Message("No challenge header".into()))?
        .to_str()
        .map_err(|_| TransportError::Message("Failed to parse challenge header".into()))?;

    rpc_client_from_websocket(ws_stream, challenge, token).await
}

#[cfg(all(feature = "remote", unix))]
pub async fn connect_unix(path: &str) -> Result<RpcClient, ClientError> {
    use probe_rs_rpc::transport::unix::{UnixStreamRx, UnixStreamTx};
    use tokio::net::UnixStream;

    let stream = UnixStream::connect(path).await.map_err(|err| {
        TransportError::Message(format!("Failed to connect to Unix socket: {err:?}"))
    })?;

    // SAFETY: `geteuid` has no preconditions, and cannot fail.
    let this_user = unsafe { libc::geteuid() };
    let server_is_verified = stream
        .peer_cred()
        .is_ok_and(|peer| peer.uid() == this_user || peer.uid() == 0);

    let (reader, writer) = stream.into_split();

    let tx = UnixStreamTx::new(writer);
    let rx = UnixStreamRx::new(reader);

    let mut client = RpcClient::new_from_wire(tx, rx).ensure_compatible().await?;
    client.server_is_verified = server_is_verified;
    Ok(client)
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

/// The right of one monitor or test call to answer the host calls of a connection.
struct HostFilesClaim<'a>(&'a AtomicBool);

impl<'a> HostFilesClaim<'a> {
    fn new(in_use: &'a AtomicBool) -> Result<Self, ClientError> {
        in_use
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ClientError::HostFilesInUse)?;
        Ok(Self(in_use))
    }
}

impl Drop for HostFilesClaim<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Websocket-backed connection to a remote probe-rs server.
#[derive(Clone)]
pub struct RpcClient {
    client: HostClient<WireError>,
    upload_cache: Arc<Mutex<UploadCache>>,
    is_localhost: bool,
    /// The connection proved that the peer is the server that the user wants.
    server_is_verified: bool,
    /// A monitor or test call answers host calls. The connection has one host call topic, so only
    /// one call at a time can.
    host_files_in_use: Arc<AtomicBool>,
    closer: Option<Closer>,
}

impl Drop for RpcClient {
    fn drop(&mut self) {
        if Arc::strong_count(&self.upload_cache) == 1 {
            // Dropping the last client
            self.client.close();
        }
    }
}

/// In local mode, the server opens the files of the client.
fn local_server_path(path: &Path) -> ServerPath {
    ServerPath::new(path.display().to_string())
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
                TokioSpawner::default(),
                &HostClientConfig {
                    seq_kind: VarSeqKind::Seq2,
                    err_uri_path: "error",
                    outgoing_depth: 1,
                    subscriber_timeout_if_full: Duration::from_secs(1),
                },
            ),
            upload_cache: Arc::new(Mutex::new(UploadCache::default())),
            is_localhost: false,
            server_is_verified: false,
            host_files_in_use: Arc::new(AtomicBool::new(false)),
            closer: None,
        }
    }

    /// Sets the handle that [`RpcClient::close`] uses to close the connection.
    pub fn with_closer(mut self, closer: Closer) -> Self {
        self.closer = Some(closer);
        self
    }

    /// Closes the connection for all clones of this client, and waits until the close message is sent.
    pub async fn close(self) {
        if let Some(closer) = &self.closer
            && tokio::time::timeout(Duration::from_secs(2), closer.close())
                .await
                .is_err()
        {
            // A hung server must not stop the command from ending.
            tracing::debug!("Timed out while closing the connection");
        }
        self.client.close();
    }

    pub fn is_local_session(&self) -> bool {
        self.is_localhost
    }

    /// Returns `true` if this client can trust the server with its files.
    ///
    /// This is true for a local server, for a unix socket with a peer of this user or root, and for
    /// an `ssh://` connection after a server proof. ssh verifies only the host, and any local user
    /// of that host can listen on the port. It is not true for `ws://` and `wss://`, because the
    /// client does not verify a certificate, and an attacker can relay a proof.
    ///
    /// The proof is only as strong as the token. A fake server on the port chooses the challenge
    /// that the client hashes with the token at login, so it can guess a weak token offline.
    pub fn server_is_trusted(&self) -> bool {
        self.is_localhost || self.server_is_verified
    }

    /// Asks the server to prove that it knows `token`.
    #[cfg(feature = "remote")]
    async fn verify_server_proof(&self, challenge: &str, token: &str) -> bool {
        let mut nonce = [0; 32];
        if getrandom::fill(&mut nonce).is_err() {
            return false;
        }

        match self
            .send_resp::<ServerProofEndpoint, _>(&ServerProofRequest {
                nonce: nonce.to_vec(),
            })
            .await
        {
            // ssh connects to the server from its own host.
            Ok(proof) => proof == server_proof(&nonce, challenge, true, token),
            Err(error) => {
                tracing::debug!("The server did not prove itself: {error:?}");
                false
            }
        }
    }

    /// Runs the server proof of an `ssh://` connection, and records the result. Only the ssh
    /// transport calls this: on other transports, a proof does not show who the server is.
    #[doc(hidden)]
    #[cfg(feature = "remote")]
    pub async fn with_server_proof(mut self, challenge: &str, token: Option<&str>) -> Self {
        // Without a token, there is no shared secret to prove.
        self.server_is_verified = match token {
            Some(token) if !token.is_empty() => self.verify_server_proof(challenge, token).await,
            _ => false,
        };
        if !self.server_is_verified {
            tracing::debug!("The server is not verified.");
        }
        self
    }

    /// Creates a client of an in-process server. The client trusts the server with its files, so
    /// the wire must lead to a server of this process.
    pub fn new_local_from_wire(
        tx: impl PostcardSender + Send + Sync + 'static,
        rx: impl PostcardReceiver + Send + 'static,
    ) -> RpcClient {
        let mut this = Self::new_from_wire(tx, rx);
        this.is_localhost = true;
        this
    }

    /// Fetch the schema of the server and refuse to continue when it does
    /// not match the schema of this client.
    pub async fn ensure_compatible(self) -> Result<Self, ClientError> {
        self.check_compatibility().await?;
        Ok(self)
    }

    async fn get_schema_report(
        &self,
        expected: &SchemaReport,
    ) -> Result<SchemaReport, SchemaError<WireError>> {
        // Our own copy of hostClient::get_schema_report so that we can tune capacity.
        // The subscription drops the oldest message when it is full, so it must hold
        // every message of a matching server.
        let expected_messages = expected.types.len()
            + expected.endpoints.len()
            + expected.topics_in.len()
            + expected.topics_out.len();
        let Ok(mut sub) = self
            .client
            .subscribe_multi::<GetAllSchemaDataTopic>(expected_messages)
            .await
        else {
            return Err(SchemaError::Comms(HostErr::Closed));
        };

        let receive = |message| match message {
            Ok(data) => Ok(data),
            Err(MultiSubRxError::IoClosed) => Err(SchemaError::Comms(HostErr::Closed)),
            Err(MultiSubRxError::Lagged(_)) => Err(SchemaError::LostData),
        };

        // Receive the data while the request runs, so that the subscription does not fill up.
        let mut data = vec![];
        let mut request = std::pin::pin!(self.client.send_resp::<GetAllSchemasEndpoint>(&()));
        let resp = loop {
            tokio::select! {
                resp = &mut request => break resp.map_err(SchemaError::Comms)?,
                message = sub.recv() => data.push(receive(message)?),
            }
        };

        // The server sends the response after the data, so the rest of the data
        // is already in the subscription. The timeout covers a message that the
        // subscription cannot decode.
        let sent =
            resp.types_sent + resp.endpoints_sent + resp.topics_in_sent + resp.topics_out_sent;
        while data.len() < sent as usize {
            match tokio::time::timeout(Duration::from_millis(500), sub.recv()).await {
                Ok(message) => data.push(receive(message)?),
                Err(_) => return Err(SchemaError::LostData),
            }
        }

        let mut rpt = SchemaReport::default();
        let mut e_and_t = vec![];

        for d in data {
            match d {
                OwnedSchemaData::Type(d) => {
                    rpt.add_type(d);
                }
                e @ OwnedSchemaData::Endpoint { .. } => e_and_t.push(e),
                t @ OwnedSchemaData::Topic { .. } => e_and_t.push(t),
            }
        }

        for e in e_and_t {
            match e {
                OwnedSchemaData::Type(_) => unreachable!(),
                OwnedSchemaData::Endpoint {
                    path,
                    request_key,
                    response_key,
                } => {
                    rpt.add_endpoint(path, request_key, response_key)?;
                }
                OwnedSchemaData::Topic {
                    path,
                    key,
                    direction,
                } => match direction {
                    TopicDirection::ToServer => rpt.add_topic_in(path, key)?,
                    TopicDirection::ToClient => rpt.add_topic_out(path, key)?,
                },
            }
        }

        let mut data_matches = true;
        data_matches &= resp.endpoints_sent as usize == rpt.endpoints.len();
        data_matches &= resp.topics_in_sent as usize == rpt.topics_in.len();
        data_matches &= resp.topics_out_sent as usize == rpt.topics_out.len();
        data_matches &= resp.errors == 0;

        if data_matches {
            // TODO: filter primitive types out?
            Ok(rpt)
        } else {
            Err(SchemaError::LostData)
        }
    }

    /// Fetch the schema of the server and compare it with the schema of this
    /// client.
    pub async fn check_compatibility(&self) -> Result<(), ClientError> {
        let expected = schema::expected_schema_report()?;
        let actual = self
            .get_schema_report(&expected)
            .await
            .map_err(schema::from_schema_err)?;

        if schema::schema_reports_match(&expected, &actual) {
            Ok(())
        } else {
            Err(ClientError::IncompatibleServer)
        }
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

    /// Returns an error if `host_files` must not serve this server.
    pub fn check_host_files(&self, host_files: &HostFiles) -> Result<(), ClientError> {
        if self.server_is_trusted() || host_files.allows_unverified_server() {
            Ok(())
        } else {
            Err(ClientError::UnverifiedServer)
        }
    }

    /// Like [`Self::send_and_read_stream`], for a request that runs the target. It also answers the
    /// host calls of the server, in a task of its own.
    async fn run_monitored<E, R>(
        &self,
        req: &E::Request,
        host_files: Option<HostFiles>,
        mut on_msg: impl AsyncFnMut(MonitorEvent),
    ) -> Result<R, ClientError>
    where
        E: postcard_rpc::Endpoint<Response = RpcResult<R>>,
        E::Request: Serialize + Schema,
        E::Response: DeserializeOwned + Schema,
    {
        /// More than the window of the server: 16 posted writes and one other call. So this queue
        /// does not drop a call.
        const HOST_CALL_DEPTH: usize = 32;
        /// The output that waits for `on_msg`. If the terminal is too slow, output gets dropped.
        /// Otherwise a full subscription stalls every message of the connection.
        const EVENT_QUEUE_DEPTH: usize = 4096;

        let _host_files_claim = match &host_files {
            Some(host_files) => {
                self.check_host_files(host_files)?;
                Some(HostFilesClaim::new(&self.host_files_in_use)?)
            }
            None => None,
        };

        let mut events = <MonitorEvent as MultiTopic>::subscribe(&self.client, 64).await?;
        let mut host_calls = match &host_files {
            Some(_) => {
                Some(<HostCallTopic as MultiTopic>::subscribe(&self.client, HOST_CALL_DEPTH).await?)
            }
            None => None,
        };

        let stop_events = Arc::new(Notify::new());
        let stop_host_calls = Arc::new(Notify::new());
        let (queue, mut queued) = tokio::sync::mpsc::channel(EVENT_QUEUE_DEPTH);

        let request = async {
            let result = self.send_resp::<E, R>(req).await;
            stop_events.notify_one();
            stop_host_calls.notify_one();
            result
        };
        let read_events = events.forward(queue, stop_events.clone());
        let handle_events = async {
            while let Some(event) = queued.recv().await {
                on_msg(event).await;
            }
        };
        let answer_host_calls = async {
            if let (Some(host_files), Some(host_calls)) = (&host_files, &mut host_calls) {
                host_files
                    .serve(self, host_calls, stop_host_calls.clone())
                    .await;
            }
        };

        let (_, _, result, _) =
            tokio::join!(read_events, handle_events, request, answer_host_calls);
        result
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
                remote_path: local_server_path(&src_path),
                canonical_path: src_path,
                content_hash,
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
    pub async fn upload_file(&self, src_path: &Path) -> Result<ServerPath, ClientError> {
        if self.is_localhost {
            let src_path = src_path
                .canonicalize()
                .unwrap_or_else(|_| src_path.to_path_buf());
            return Ok(local_server_path(&src_path));
        }

        Ok(self.resolve_upload(src_path).await?.remote_path)
    }

    async fn upload_bytes(&self, src_path: &Path, data: &[u8]) -> Result<ServerPath, ClientError> {
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
        Ok(ServerPath::new(path))
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

    /// Resets the target with the reset line of the probe or through the debug interface. The
    /// probe settings select the method if the request does not.
    pub async fn reset_target(
        &self,
        request: ResetTargetRequest,
    ) -> Result<AttachResult<()>, ClientError> {
        self.send_resp::<ResetTargetEndpoint, _>(&request).await
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
        rtt_client: Option<Key<RttClient>>,
    ) -> Result<BuildResult, ClientError> {
        let upload = self.client.resolve_upload(&path).await?;
        self.build_flash_loader_resolved(
            &upload,
            format,
            image_target,
            read_flasher_rtt,
            rtt_client,
        )
        .await
    }

    pub async fn build_flash_loader_resolved(
        &self,
        upload: &ResolvedUpload,
        format: FormatOptions,
        image_target: Option<String>,
        read_flasher_rtt: bool,
        rtt_client: Option<Key<RttClient>>,
    ) -> Result<BuildResult, ClientError> {
        self.client
            .send_resp::<BuildEndpoint, _>(&BuildRequest {
                sessid: self.sessid,
                path: upload.server_path().clone(),
                format,
                image_target,
                read_flasher_rtt,
                rtt_client,
            })
            .await
    }

    pub async fn flash(
        &self,
        options: DownloadOptions,
        loader: Key<FlashLoader>,
        on_msg: impl AsyncFnMut(ProgressEvent),
    ) -> Result<(), ClientError> {
        self.client
            .send_and_read_stream::<FlashEndpoint, ProgressEventTopic, _>(
                &FlashRequest {
                    sessid: self.sessid,
                    loader,
                    options,
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
        mut options: MonitorOptions,
        host_files: Option<HostFiles>,
        on_msg: impl AsyncFnMut(MonitorEvent),
    ) -> Result<MonitorExitReason, ClientError> {
        options.host_calls = host_files.is_some();
        self.client
            .run_monitored::<MonitorEndpoint, _>(
                &MonitorRequest {
                    sessid: self.sessid,
                    mode,
                    options,
                },
                host_files,
                on_msg,
            )
            .await
    }

    /// Writes to the serial port to the target. Returns the number of bytes written.
    pub async fn send_to_serial(&self, data: Vec<u8>) -> Result<u32, ClientError> {
        self.client
            .send_resp::<SerialWriteEndpoint, _>(&SerialWriteRequest {
                sessid: self.sessid,
                data,
            })
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
        host_files: Option<HostFiles>,
        on_msg: impl AsyncFnMut(MonitorEvent),
    ) -> Result<Tests, ClientError> {
        self.client
            .run_monitored::<ListTestsEndpoint, _>(
                &ListTestsRequest {
                    sessid: self.sessid,
                    boot_info,
                    rtt_client,
                    host_calls: host_files.is_some(),
                },
                host_files,
                on_msg,
            )
            .await
    }

    pub async fn run_test(
        &self,
        test: Test,
        rtt_client: Option<Key<RttClient>>,
        host_files: Option<HostFiles>,
        on_msg: impl AsyncFnMut(MonitorEvent),
    ) -> Result<TestResult, ClientError> {
        self.client
            .run_monitored::<RunTestEndpoint, _>(
                &RunTestRequest {
                    sessid: self.sessid,
                    test,
                    rtt_client,
                    host_calls: host_files.is_some(),
                },
                host_files,
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
                path,
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
                path: upload.server_path().clone(),
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
                path: Some(path),
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
    /// Writes `data` with 32-bit accesses where the address and length allow.
    pub async fn write_bytes(&self, address: u64, data: Vec<u8>) -> Result<(), ClientError> {
        self.client
            .send_resp::<WriteBytesEndpoint, _>(&WriteMemoryRequest {
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

    /// Like [`Self::stream`], but moves the messages to `queue`. It drops a message if `queue` is
    /// full, so a slow reader of `queue` cannot stall the connection.
    async fn forward(
        &mut self,
        queue: tokio::sync::mpsc::Sender<Self::Message>,
        stopper: Arc<Notify>,
    ) {
        // One warning for each run of dropped messages, so that a slow terminal does not also
        // flood the log.
        let dropped = std::sync::atomic::AtomicUsize::new(0);
        let enqueue = |message| match queue.try_send(message) {
            Ok(()) => {
                let count = dropped.swap(0, Ordering::Relaxed);
                if count > 0 {
                    tracing::warn!("Dropped {count} messages of target output.");
                }
            }
            Err(_) => {
                if dropped.fetch_add(1, Ordering::Relaxed) == 0 {
                    tracing::warn!("The output is too slow. Dropping target output.");
                }
            }
        };

        let listen_fut = async {
            while let Some(message) = self.next().await {
                enqueue(message);
            }
        };

        tokio::select! {
            _ = listen_fut => {}
            _ = stopper.notified() => {
                while let Ok(Some(message)) = timeout(Duration::from_millis(100), self.next()).await {
                    enqueue(message);
                }
            }
        }
    }

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

#[derive(Debug)]
pub enum MonitorEvent {
    Rtt(RttEvent),
    Semihosting(SemihostingEvent),
    Serial(SerialEvent),
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
        let serial = SerialTopic::subscribe(client, depth).await?;
        Ok(MonitorSubscription {
            rtt,
            semihosting,
            serial,
        })
    }
}

pub(crate) struct MonitorSubscription {
    rtt: <RttTopic as MultiTopic>::Subscription,
    semihosting: <SemihostingTopic as MultiTopic>::Subscription,
    serial: <SerialTopic as MultiTopic>::Subscription,
}
impl MultiSubscription for MonitorSubscription {
    type Message = MonitorEvent;

    async fn next(&mut self) -> Option<Self::Message> {
        tokio::select! {
            message = self.rtt.recv() => message.map(MonitorEvent::Rtt),
            message = self.semihosting.recv() => message.map(MonitorEvent::Semihosting),
            message = self.serial.recv() => message.map(MonitorEvent::Serial),
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

#[cfg(all(test, feature = "remote"))]
mod tests {
    use super::{RpcClient, without_userinfo, worker_uri};
    use futures_util::StreamExt as _;
    use std::time::Duration;
    use tokio::io::DuplexStream;
    use tokio_tungstenite::{
        WebSocketStream,
        tungstenite::{Message, protocol::Role},
    };

    /// A second monitor call on the connection must not take the host calls of the first one.
    #[test]
    fn host_files_have_one_user_at_a_time() {
        use super::{ClientError, HostFilesClaim};

        let in_use = std::sync::atomic::AtomicBool::new(false);
        let first = HostFilesClaim::new(&in_use).unwrap();
        assert!(matches!(
            HostFilesClaim::new(&in_use),
            Err(ClientError::HostFilesInUse)
        ));

        drop(first);
        assert!(HostFilesClaim::new(&in_use).is_ok());
    }

    fn websocket_client(client_ws: WebSocketStream<DuplexStream>) -> RpcClient {
        use probe_rs_rpc::transport::websocket::{WebsocketRx, WebsocketTx};
        use tokio_util::bytes::Bytes;

        let (tx, rx) = client_ws.split();
        let tx = WebsocketTx::new(tx);
        let closer = tx.closer();
        RpcClient::new_from_wire(
            tx,
            WebsocketRx::new(rx.map(|message| {
                message.map(|message| match message {
                    Message::Binary(binary) => binary,
                    _ => Bytes::new(),
                })
            })),
        )
        .with_closer(closer)
    }

    /// Reads the server side of the websocket until it ends, and checks for a close frame.
    async fn assert_close_frame(mut server_ws: WebSocketStream<DuplexStream>) {
        let messages = tokio::time::timeout(Duration::from_secs(5), async {
            let mut messages = Vec::new();
            while let Some(message) = server_ws.next().await {
                messages.push(format!("{message:?}"));
            }
            messages
        })
        .await
        .expect("the websocket stayed open");

        assert!(
            messages
                .iter()
                .any(|message| message.starts_with("Ok(Close(")),
            "no close frame, the server got: {messages:?}"
        );
    }

    /// The client must end the websocket with a close frame, so that the
    /// server can tell a clean exit from a lost connection.
    #[tokio::test]
    async fn dropping_the_client_closes_the_websocket() {
        let (client_io, server_io) = tokio::io::duplex(4096);
        let client_ws = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let server_ws = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;

        drop(websocket_client(client_ws));

        assert_close_frame(server_ws).await;
    }

    /// A CLI command ends its runtime right after it is done with the client.
    /// The close frame must be sent before that.
    #[test]
    fn the_close_frame_is_sent_before_the_runtime_stops() {
        let (client_io, server_io) = tokio::io::duplex(4096);

        let client_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        client_runtime.block_on(async {
            let client_ws = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
            websocket_client(client_ws).close().await;
        });
        drop(client_runtime);

        let server_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        server_runtime.block_on(async {
            let server_ws = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
            assert_close_frame(server_ws).await;
        });
    }

    #[test]
    fn a_websocket_path_comes_before_the_worker_endpoint() {
        for (connection_string, uri) in [
            ("ws://host:3000", "ws://host:3000/worker"),
            ("ws://host:3000/", "ws://host:3000/worker"),
            ("wss://host/probe", "wss://host/probe/worker"),
            ("wss://host/probe/", "wss://host/probe/worker"),
            ("ws://host//", "ws://host/worker"),
            ("WSS://host/probe", "wss://host/probe/worker"),
            ("ws://user@host:3000", "ws://user@host:3000/worker"),
        ] {
            assert_eq!(worker_uri(connection_string).unwrap().to_string(), uri);
        }
    }

    #[test]
    fn an_error_shows_the_uri_without_the_password() {
        let uri = worker_uri("wss://user:secret@host:3000/probe").unwrap();
        assert_eq!(without_userinfo(&uri), "wss://host:3000/probe/worker");
    }

    #[test]
    fn other_connection_strings_are_refused() {
        for connection_string in [
            "http://host:3000",
            "HTTP://host:3000",
            "host:3000",
            "ws://:3000",
            "ws://host/probe?token=1",
        ] {
            assert!(
                worker_uri(connection_string).is_err(),
                "{connection_string}"
            );
        }
    }
}
