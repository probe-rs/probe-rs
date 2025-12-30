//! Remote client
//!
//! The client opens a websocket connection to the host, sends a token to authenticate and
//! then sends commands to the server. The commands are handled by the server (by the same
//! handlers that are used for the local commands) and the output is streamed back to the client.
//!
//! The command output may be a result and/or a stream of messages encoded as [ServerMessage].

use postcard_rpc::{
    Topic,
    header::{VarSeq, VarSeqKind},
    host_client::{HostClient, HostClientConfig, HostErr, IoClosed, SubscribeError, Subscription},
};
use postcard_schema::Schema;
use probe_rs::{Session, config::Registry, flashing::FlashLoader};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    sync::{Mutex, MutexGuard, Notify},
    time::timeout,
};

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::{
    FormatOptions,
    rpc::{
        Key,
        functions::{
            AttachEndpoint, BuildEndpoint, ChipInfoEndpoint, CreateRttClientEndpoint,
            CreateTempFileEndpoint, EraseEndpoint, FlashEndpoint, ListChipFamiliesEndpoint,
            ListProbesEndpoint, ListTestsEndpoint, LoadChipFamilyEndpoint, MonitorEndpoint,
            ProgressEventTopic, ReadMemory8Endpoint, ReadMemory16Endpoint, ReadMemory32Endpoint,
            ReadMemory64Endpoint, ResetCoreAndHaltEndpoint, ResetCoreEndpoint,
            ResumeAllCoresEndpoint, RpcResult, RunTestEndpoint, SelectProbeEndpoint,
            TakeStackTraceEndpoint, TargetInfoDataTopic, TargetInfoEndpoint, TempFileDataEndpoint,
            TokioSpawner, VerifyEndpoint, WriteMemory8Endpoint, WriteMemory16Endpoint,
            WriteMemory32Endpoint, WriteMemory64Endpoint,
            chip::{ChipData, ChipFamily, ChipInfoRequest, LoadChipFamilyRequest},
            file::{AppendFileRequest, TempFile},
            flash::{
                BootInfo, BuildRequest, BuildResult, DownloadOptions, EraseCommand, EraseRequest,
                FlashRequest, ProgressEvent, VerifyRequest, VerifyResult,
            },
            info::{InfoEvent, TargetInfoRequest},
            memory::{ReadMemoryRequest, WriteMemoryRequest},
            monitor::{MonitorExitReason, MonitorMode, MonitorOptions, MonitorRequest},
            probe::{
                AttachRequest, AttachResult, DebugProbeEntry, DebugProbeSelector,
                ListProbesRequest, SelectProbeRequest, SelectProbeResult,
            },
            reset::{ResetCoreAndHaltRequest, ResetCoreRequest},
            resume::ResumeAllCoresRequest,
            rtt_client::{CreateRttClientRequest, RttClientData, ScanRegion},
            stack_trace::{StackTraces, TakeStackTraceRequest},
            test::{ListTestsRequest, RunTestRequest, Test, TestResult, Tests},
        },
        transport::memory::{PostcardReceiver, PostcardSender, WireRx, WireTx},
        utils::semihosting::SemihostingOptions,
    },
    util::{
        cli::MonitorEvent,
        rtt::{RttChannelConfig, client::RttClient},
    },
};

#[cfg(feature = "remote")]
pub async fn connect(host: &str, token: Option<String>) -> anyhow::Result<RpcClient> {
    use crate::rpc::transport::websocket::{WebsocketRx, WebsocketTx};
    use anyhow::Context;
    use axum::http::Uri;
    use futures_util::StreamExt as _;
    use rustls::ClientConfig;
    use sha2::{Digest, Sha512};
    use std::str::FromStr;
    use tokio_tungstenite::{
        connect_async_tls_with_config,
        tungstenite::{ClientRequestBuilder, Message},
    };
    use tokio_util::bytes::Bytes;

    let uri = Uri::from_str(&format!("{host}/worker")).context("Failed to parse server URI")?;

    // We could check the host address for localhost and then set the `is_localhost` option, but
    // there are setups where the user uses port forwarding and the file actually needs to be
    // uploaded for correct behavior. Therefore, this check is not performed.

    let req = ClientRequestBuilder::new(uri).with_header(
        "User-Agent",
        format!("probe-rs-tools {}", env!("PROBE_RS_LONG_VERSION")),
    );

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
    .context("Failed to connect")?;

    // Respond to the challenge
    let challenge = resp
        .headers()
        .get("Probe-Rs-Challenge")
        .context("No challenge header")?
        .to_str()
        .context("Failed to parse challenge header")?;

    let mut hasher = Sha512::new();
    hasher.update(challenge.as_bytes());
    hasher.update(token.unwrap_or_default().as_bytes());
    let challenge_response = hasher.finalize().to_vec();

    let (tx, rx) = ws_stream.split();

    let tx = WebsocketTx::new(tx);
    tx.send(challenge_response)
        .await
        .map_err(|err| anyhow::anyhow!("Failed to send challenge response: {err:?}"))?;

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

/// Represents a connection to a remote server.
///
/// Internally implemented as a websocket connection.
#[derive(Clone)]
pub struct RpcClient {
    client: HostClient<String>,
    uploaded_files: Arc<Mutex<HashMap<PathBuf, PathBuf>>>,
    registry: Arc<Mutex<Registry>>,
    is_localhost: bool,
}

impl Drop for RpcClient {
    fn drop(&mut self) {
        if Arc::strong_count(&self.uploaded_files) == 1 {
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
            client: HostClient::<String>::new_with_wire_and_config(
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
            uploaded_files: Arc::new(Mutex::new(HashMap::new())),
            registry: Arc::new(Mutex::new(Registry::from_builtin_families())),
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

    async fn send<E, T>(&self, req: &E::Request) -> anyhow::Result<T>
    where
        E: postcard_rpc::Endpoint<Response = T>,
        E::Request: Serialize + Schema,
        E::Response: DeserializeOwned + Schema,
    {
        match self.client.send_resp::<E>(req).await {
            Ok(r) => Ok(r),
            Err(e) => match e {
                HostErr::Wire(w) => anyhow::bail!("Wire error: {w}"),
                HostErr::BadResponse => anyhow::bail!("Bad response"),
                HostErr::Postcard(error) => anyhow::bail!("Postcard error: {error}"),
                HostErr::Closed => anyhow::bail!("Connection closed"),
            },
        }
    }

    async fn send_resp<E, T>(&self, req: &E::Request) -> anyhow::Result<T>
    where
        E: postcard_rpc::Endpoint<Response = RpcResult<T>>,
        E::Request: Serialize + Schema,
        E::Response: DeserializeOwned + Schema,
    {
        match self.send::<E, RpcResult<T>>(req).await? {
            Ok(r) => Ok(r),
            Err(e) => anyhow::bail!("{e}"),
        }
    }

    pub async fn publish<T: Topic>(&self, message: &T::Message) -> Result<(), IoClosed>
    where
        T::Message: Serialize,
    {
        self.client.publish::<T>(VarSeq::Seq2(0), message).await
    }

    async fn send_and_read_stream<E, T, R>(
        &self,
        req: &E::Request,
        on_msg: impl AsyncFnMut(T::Message),
    ) -> anyhow::Result<R>
    where
        E: postcard_rpc::Endpoint<Response = RpcResult<R>>,
        E::Request: Serialize + Schema,
        E::Response: DeserializeOwned + Schema,
        T: MultiTopic,
    {
        let mut stream = match T::subscribe(&self.client, 64).await {
            Ok(stream) => stream,
            Err(err) => anyhow::bail!("Failed to subscribe to '{}': {:?}", err.topic, err.error),
        };
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

    pub async fn upload_file(&self, src_path: impl AsRef<Path>) -> anyhow::Result<PathBuf> {
        use anyhow::Context as _;

        let src_path = src_path
            .as_ref()
            .canonicalize()
            .unwrap_or_else(|_| src_path.as_ref().to_path_buf());

        if self.is_localhost {
            return Ok(src_path);
        }

        let mut uploaded = self.uploaded_files.lock().await;
        if let Some(path) = uploaded.get(&src_path) {
            return Ok(path.clone());
        }

        let data = tokio::fs::read(&src_path)
            .await
            .context("Failed to read file")?;
        tracing::debug!("Uploading {} ({} bytes)", src_path.display(), data.len());

        let TempFile { key, path } = self.send_resp::<CreateTempFileEndpoint, _>(&()).await?;

        for chunk in data.chunks(1024 * 512) {
            self.send_resp::<TempFileDataEndpoint, _>(&AppendFileRequest {
                data: chunk.into(),
                key,
            })
            .await?;
        }

        tracing::debug!("Uploaded file to {}", path);
        let path = PathBuf::from(path);
        uploaded.insert(src_path, path.clone());

        Ok(path)
    }

    pub async fn attach_probe(&self, request: AttachRequest) -> anyhow::Result<AttachResult> {
        self.send_resp::<AttachEndpoint, _>(&request).await
    }

    pub async fn list_probes(&self) -> anyhow::Result<Vec<DebugProbeEntry>> {
        self.send_resp::<ListProbesEndpoint, _>(&ListProbesRequest::all())
            .await
    }

    pub async fn select_probe(
        &self,
        selector: Option<DebugProbeSelector>,
    ) -> anyhow::Result<SelectProbeResult> {
        self.send_resp::<SelectProbeEndpoint, _>(&SelectProbeRequest { probe: selector })
            .await
    }

    pub async fn info(
        &self,
        request: TargetInfoRequest,
        on_msg: impl AsyncFnMut(InfoEvent),
    ) -> anyhow::Result<()> {
        self.send_and_read_stream::<TargetInfoEndpoint, TargetInfoDataTopic, _>(&request, on_msg)
            .await
    }

    pub async fn load_chip_family(&self, families_yaml: String) -> anyhow::Result<()> {
        self.send_resp::<LoadChipFamilyEndpoint, _>(&LoadChipFamilyRequest { families_yaml })
            .await
    }

    pub async fn list_chip_families(&self) -> anyhow::Result<Vec<ChipFamily>> {
        self.send_resp::<ListChipFamiliesEndpoint, _>(&()).await
    }

    pub async fn chip_info(&self, name: &str) -> anyhow::Result<ChipData> {
        self.send_resp::<ChipInfoEndpoint, _>(&ChipInfoRequest { name: name.into() })
            .await
    }

    pub(crate) async fn registry(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().await
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

    pub fn core(&self, core: usize) -> CoreInterface {
        CoreInterface {
            sessid: self.sessid,
            core: core as u32,
            client: self.client.clone(),
        }
    }

    pub async fn resume_all_cores(&self) -> anyhow::Result<()> {
        self.client
            .send_resp::<ResumeAllCoresEndpoint, _>(&ResumeAllCoresRequest {
                sessid: self.sessid,
            })
            .await
    }

    pub async fn build_flash_loader(
        &self,
        mut path: PathBuf,
        mut format: FormatOptions,
        image_target: Option<String>,
        read_flasher_rtt: bool,
    ) -> anyhow::Result<BuildResult> {
        path = self.client.upload_file(&path).await?;

        if let Some(ref mut idf_bootloader) = format.idf_options.idf_bootloader {
            *idf_bootloader = self
                .client
                .upload_file(&*idf_bootloader)
                .await?
                .display()
                .to_string();
        }

        if let Some(ref mut idf_partition_table) = format.idf_options.idf_partition_table {
            *idf_partition_table = self
                .client
                .upload_file(&*idf_partition_table)
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
    ) -> anyhow::Result<()> {
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

    pub async fn erase(
        &self,
        command: EraseCommand,
        read_flasher_rtt: bool,
        on_msg: impl AsyncFnMut(ProgressEvent),
    ) -> anyhow::Result<()> {
        self.client
            .send_and_read_stream::<EraseEndpoint, ProgressEventTopic, _>(
                &EraseRequest {
                    sessid: self.sessid,
                    command,
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
    ) -> anyhow::Result<MonitorExitReason> {
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

    pub async fn list_tests(
        &self,
        boot_info: BootInfo,
        rtt_client: Option<Key<RttClient>>,
        semihosting_options: SemihostingOptions,
        on_msg: impl AsyncFnMut(MonitorEvent),
    ) -> anyhow::Result<Tests> {
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
    ) -> anyhow::Result<TestResult> {
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
    ) -> anyhow::Result<RttClientData> {
        self.client
            .send_resp::<CreateRttClientEndpoint, _>(&CreateRttClientRequest {
                sessid: self.sessid,
                scan_regions,
                config,
            })
            .await
    }

    pub async fn stack_trace(&self, path: PathBuf) -> anyhow::Result<StackTraces> {
        let path = self.client.upload_file(&path).await?;

        self.client
            .send_resp::<TakeStackTraceEndpoint, _>(&TakeStackTraceRequest {
                sessid: self.sessid,
                path: path.display().to_string(),
            })
            .await
    }

    pub(crate) async fn verify(
        &self,
        loader: Key<FlashLoader>,
        on_msg: impl AsyncFnMut(ProgressEvent),
    ) -> anyhow::Result<VerifyResult> {
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
    pub async fn read_memory_8(&self, address: u64, count: usize) -> anyhow::Result<Vec<u8>> {
        self.client
            .send_resp::<ReadMemory8Endpoint, _>(&ReadMemoryRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                count: count as u32,
            })
            .await
    }
    pub async fn read_memory_16(&self, address: u64, count: usize) -> anyhow::Result<Vec<u16>> {
        self.client
            .send_resp::<ReadMemory16Endpoint, _>(&ReadMemoryRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                count: count as u32,
            })
            .await
    }
    pub async fn read_memory_32(&self, address: u64, count: usize) -> anyhow::Result<Vec<u32>> {
        self.client
            .send_resp::<ReadMemory32Endpoint, _>(&ReadMemoryRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                count: count as u32,
            })
            .await
    }
    pub async fn read_memory_64(&self, address: u64, count: usize) -> anyhow::Result<Vec<u64>> {
        self.client
            .send_resp::<ReadMemory64Endpoint, _>(&ReadMemoryRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                count: count as u32,
            })
            .await
    }

    pub async fn write_memory_8(&self, address: u64, data: Vec<u8>) -> anyhow::Result<()> {
        self.client
            .send_resp::<WriteMemory8Endpoint, _>(&WriteMemoryRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                data,
            })
            .await
    }
    pub async fn write_memory_16(&self, address: u64, data: Vec<u16>) -> anyhow::Result<()> {
        self.client
            .send_resp::<WriteMemory16Endpoint, _>(&WriteMemoryRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                data,
            })
            .await
    }
    pub async fn write_memory_32(&self, address: u64, data: Vec<u32>) -> anyhow::Result<()> {
        self.client
            .send_resp::<WriteMemory32Endpoint, _>(&WriteMemoryRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                data,
            })
            .await
    }
    pub async fn write_memory_64(&self, address: u64, data: Vec<u64>) -> anyhow::Result<()> {
        self.client
            .send_resp::<WriteMemory64Endpoint, _>(&WriteMemoryRequest {
                sessid: self.sessid,
                core: self.core,
                address,
                data,
            })
            .await
    }

    pub async fn reset(&self) -> anyhow::Result<()> {
        self.client
            .send_resp::<ResetCoreEndpoint, _>(&ResetCoreRequest {
                sessid: self.sessid,
                core: self.core,
            })
            .await
    }

    pub async fn reset_and_halt(&self, timeout: Duration) -> anyhow::Result<()> {
        self.client
            .send_resp::<ResetCoreAndHaltEndpoint, _>(&ResetCoreAndHaltRequest {
                sessid: self.sessid,
                core: self.core,
                timeout,
            })
            .await
    }
}

#[derive(Debug)]
pub(crate) struct MultiSubscribeError {
    topic: &'static str,
    error: SubscribeError,
}

pub(crate) trait MultiTopic {
    type Message;
    type Subscription: MultiSubscription<Message = Self::Message>;

    async fn subscribe<E>(
        client: &HostClient<E>,
        depth: usize,
    ) -> Result<Self::Subscription, MultiSubscribeError>
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
    ) -> Result<Self::Subscription, MultiSubscribeError>
    where
        E: DeserializeOwned + Schema,
    {
        match client.subscribe_exclusive::<Self>(depth).await {
            Ok(subscription) => Ok(subscription),
            Err(error) => Err(MultiSubscribeError {
                topic: T::PATH,
                error,
            }),
        }
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
    ) -> anyhow::Result<()> {
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

impl<T> MultiSubscription for Subscription<T>
where
    T: DeserializeOwned,
{
    type Message = T;

    async fn next(&mut self) -> Option<Self::Message> {
        self.recv().await
    }
}
