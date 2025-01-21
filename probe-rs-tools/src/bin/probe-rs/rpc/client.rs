//! Remote client
//!
//! The client opens a websocket connection to the host, sends a token to authenticate and
//! then sends commands to the server. The commands are handled by the server (by the same
//! handlers that are used for the local commands) and the output is streamed back to the client.
//!
//! The command output may be a result and/or a stream of messages encoded as [ServerMessage].

use anyhow::Context as _;
use postcard_rpc::{
    header::{VarSeq, VarSeqKind},
    host_client::{HostClient, HostClientConfig, HostErr, IoClosed, Subscription},
    Topic,
};
use postcard_schema::Schema;
use probe_rs::Session;
use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::Mutex;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::{
    rpc::{
        functions::{
            chip::{ChipData, ChipFamily, ChipInfoRequest, LoadChipFamilyRequest},
            file::{AppendFileRequest, TempFile},
            flash::{BootInfo, DownloadOptions, FlashRequest, FlashResult, ProgressEvent},
            info::{InfoEvent, TargetInfoRequest},
            memory::{ReadMemoryRequest, WriteMemoryRequest},
            monitor::{MonitorEvent, MonitorMode, MonitorOptions, MonitorRequest},
            probe::{
                AttachRequest, AttachResult, DebugProbeEntry, DebugProbeSelector,
                ListProbesRequest, SelectProbeRequest, SelectProbeResult,
            },
            reset::ResetCoreRequest,
            resume::ResumeAllCoresRequest,
            rtt_client::{CreateRttClientRequest, LogOptions},
            stack_trace::{StackTraces, TakeStackTraceRequest},
            test::{ListTestsRequest, RunTestRequest, Test, TestResult, Tests},
            AttachEndpoint, ChipInfoEndpoint, CreateRttClientEndpoint, CreateTempFileEndpoint,
            FlashEndpoint, ListChipFamiliesEndpoint, ListProbesEndpoint, ListTestsEndpoint,
            LoadChipFamilyEndpoint, MonitorEndpoint, MonitorTopic, ProgressEventTopic,
            ReadMemory16Endpoint, ReadMemory32Endpoint, ReadMemory64Endpoint, ReadMemory8Endpoint,
            ResetCoreEndpoint, ResumeAllCoresEndpoint, RpcResult, RunTestEndpoint,
            SelectProbeEndpoint, TakeStackTraceEndpoint, TargetInfoDataTopic, TargetInfoEndpoint,
            TempFileDataEndpoint, TokioSpawner, WriteMemory16Endpoint, WriteMemory32Endpoint,
            WriteMemory64Endpoint, WriteMemory8Endpoint,
        },
        transport::memory::{PostcardReceiver, PostcardSender, WireRx, WireTx},
        Key,
    },
    util::rtt::client::RttClient,
    FormatOptions,
};

#[cfg(feature = "remote")]
pub async fn connect(host: &str, token: Option<String>) -> anyhow::Result<RpcClient> {
    use crate::rpc::transport::websocket::{WebsocketRx, WebsocketTx};
    use axum::http::Uri;
    use futures_util::StreamExt as _;
    use sha2::{Digest, Sha512};
    use std::str::FromStr;
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{ClientRequestBuilder, Message},
    };
    use tokio_util::bytes::Bytes;

    let uri = Uri::from_str(&format!("{}/worker", host)).context("Failed to parse server URI")?;

    let is_localhost = uri
        .host()
        .is_some_and(|h| ["localhost", "127.0.0.1", "::1"].contains(&h));
    let req = ClientRequestBuilder::new(uri).with_header(
        "User-Agent",
        format!("probe-rs-tools {}", env!("PROBE_RS_LONG_VERSION")),
    );

    let (ws_stream, resp) = connect_async(req).await.context("Failed to connect")?;
    let (tx, rx) = ws_stream.split();

    let tx = WebsocketTx::new(tx);

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
    let result = hasher.finalize().to_vec();

    tx.send(result)
        .await
        .map_err(|err| anyhow::anyhow!("Failed to send challenge response: {:?}", err))?;

    let mut client = RpcClient::new_from_wire(
        tx,
        WebsocketRx::new(rx.map(|message| {
            message.map(|message| match message {
                Message::Binary(binary) => binary,
                _ => Bytes::new(),
            })
        })),
    );
    client.is_localhost = is_localhost;

    Ok(client)
}

/// Represents a connection to a remote server.
///
/// Internally implemented as a websocket connection.
#[derive(Clone)]
pub struct RpcClient {
    client: HostClient<String>,
    uploaded_files: Arc<Mutex<HashMap<PathBuf, PathBuf>>>,
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
            is_localhost: false,
        }
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
                HostErr::Wire(w) => anyhow::bail!("Wire error: {}", w),
                HostErr::BadResponse => anyhow::bail!("Bad response"),
                HostErr::Postcard(error) => anyhow::bail!("Postcard error: {}", error),
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
            Err(e) => anyhow::bail!("{}", e),
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
        on_msg: impl FnMut(T::Message),
    ) -> anyhow::Result<R>
    where
        E: postcard_rpc::Endpoint<Response = RpcResult<R>>,
        E::Request: Serialize + Schema,
        E::Response: DeserializeOwned + Schema,
        T: Topic,
        T::Message: DeserializeOwned,
    {
        let mut stream = match self.client.subscribe_exclusive::<T>(64).await {
            Ok(stream) => stream,
            Err(err) => anyhow::bail!("Failed to subscribe to '{}': {err:?}", T::PATH),
        };

        tokio::select! {
            biased;
            _ = read_stream(&mut stream, on_msg) => anyhow::bail!("Topic reader returned unexpectedly"),
            r = self.send_resp::<E, R>(req) => r,
        }
    }

    pub async fn upload_file(&self, src_path: impl AsRef<Path>) -> anyhow::Result<PathBuf> {
        use anyhow::Context as _;

        let src_path = src_path.as_ref().canonicalize()?;
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
        on_msg: impl FnMut(InfoEvent),
    ) -> anyhow::Result<()> {
        self.send_and_read_stream::<TargetInfoEndpoint, TargetInfoDataTopic, _>(&request, on_msg)
            .await
    }

    pub async fn load_chip_family(
        &self,
        families: probe_rs_target::ChipFamily,
    ) -> anyhow::Result<()> {
        // I refuse to add a schema to ChipFamily until we can actually load it on the server.
        let family = postcard::to_stdvec(&families).context("Failed to serialize chip family")?;

        self.send_resp::<LoadChipFamilyEndpoint, _>(&LoadChipFamilyRequest {
            family_data: family,
        })
        .await
    }

    pub async fn list_chip_families(&self) -> anyhow::Result<Vec<ChipFamily>> {
        self.send_resp::<ListChipFamiliesEndpoint, _>(&()).await
    }

    pub async fn chip_info(&self, name: &str) -> anyhow::Result<ChipData> {
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
    pub fn new(iface: RpcClient, sessid: Key<Session>) -> Self {
        Self {
            sessid,
            client: iface,
        }
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

    pub async fn flash(
        &self,
        mut path: PathBuf,
        mut format: FormatOptions,
        options: DownloadOptions,
        rtt_client: Option<Key<RttClient>>,
        on_msg: impl FnMut(ProgressEvent),
    ) -> anyhow::Result<FlashResult> {
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
            .send_and_read_stream::<FlashEndpoint, ProgressEventTopic, _>(
                &FlashRequest {
                    sessid: self.sessid,
                    path: path.display().to_string(),
                    format,
                    options,
                    rtt_client,
                },
                on_msg,
            )
            .await
    }

    pub async fn monitor(
        &self,
        mode: MonitorMode,
        options: MonitorOptions,
        on_msg: impl FnMut(MonitorEvent),
    ) -> anyhow::Result<()> {
        self.client
            .send_and_read_stream::<MonitorEndpoint, MonitorTopic, _>(
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
        on_msg: impl FnMut(MonitorEvent),
    ) -> anyhow::Result<Tests> {
        self.client
            .send_and_read_stream::<ListTestsEndpoint, MonitorTopic, _>(
                &ListTestsRequest {
                    sessid: self.sessid,
                    boot_info,
                    rtt_client,
                },
                on_msg,
            )
            .await
    }

    pub async fn run_test(
        &self,
        test: Test,
        rtt_client: Option<Key<RttClient>>,
        on_msg: impl FnMut(MonitorEvent),
    ) -> anyhow::Result<TestResult> {
        self.client
            .send_and_read_stream::<RunTestEndpoint, MonitorTopic, _>(
                &RunTestRequest {
                    sessid: self.sessid,
                    test,
                    rtt_client,
                },
                on_msg,
            )
            .await
    }

    pub async fn create_rtt_client(
        &self,
        mut path: Option<PathBuf>,
        log_options: LogOptions,
    ) -> anyhow::Result<Key<RttClient>> {
        // TODO: RTT shouldn't need the firmware, we can extract location and process output locally
        if let Some(ref mut path) = path {
            *path = self.client.upload_file(&*path).await?;
        }

        self.client
            .send_resp::<CreateRttClientEndpoint, _>(&CreateRttClientRequest {
                sessid: self.sessid,
                path: path.map(|path| path.display().to_string()),
                log_options,
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
}

async fn read_stream<T>(stream: &mut Subscription<T>, mut on_msg: impl FnMut(T))
where
    T: DeserializeOwned,
{
    while let Some(message) = stream.recv().await {
        on_msg(message);
    }

    tracing::warn!("Failed to read topic");
    futures_util::future::pending().await
}
