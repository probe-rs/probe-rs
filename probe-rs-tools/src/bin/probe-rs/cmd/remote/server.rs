//! Remote server
//!
//! The server listens for incoming websocket connections and executes commands on behalf of the
//! client. The server also provides a status webpage that shows the available probes.
//!
//! The commands are executed in separate processes and the output is streamed back to the client.
//! The server tracks opened probes and ensures that only one command is executed per probe at a time.

use anyhow::Context as _;
use axum::{
    extract::{
        ws::{self, WebSocket},
        State, WebSocketUpgrade,
    },
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use axum_extra::{
    headers::{self, authorization},
    TypedHeader,
};
use futures_util::future::join3;
use probe_rs::probe::{list::Lister, DebugProbeSelector};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tokio::{io::AsyncBufReadExt as _, sync::Mutex};

use std::{
    collections::HashMap,
    fmt::Write,
    future::Future,
    io::Write as _,
    process::Stdio,
    sync::Arc,
    task::{Poll, Waker},
};

use super::{ClientMessage, ServerMessage};
use crate::{Cli, Config};

struct ServerState {
    config: Config,
    open_devices: Mutex<HashMap<DebugProbeSelector, Vec<Waker>>>,
}

impl ServerState {
    fn new(config: Config) -> Self {
        Self {
            config,
            open_devices: Mutex::new(HashMap::new()),
        }
    }

    async fn device_opened(&self, probe: &DebugProbeSelector) {
        loop {
            // Scope to drop the lock before awaiting
            {
                let mut open_devices = self.open_devices.lock().await;

                let Some(wakers) = open_devices.get_mut(probe) else {
                    // We are the first to access this device
                    open_devices.insert(probe.clone(), Vec::new());
                    return;
                };

                // add waker to the wait list
                std::future::poll_fn(|cx| {
                    // avoid storing duplicate wakers
                    if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
                        wakers.push(cx.waker().clone());
                    }
                    Poll::Ready(())
                })
                .await;
            }

            // yield once without waking ourselves
            let mut called = false;
            std::future::poll_fn(|_cx| {
                if called {
                    return Poll::Ready(());
                }
                called = true;
                Poll::Pending
            })
            .await;
        }
    }

    async fn device_closed(&self, probe: &DebugProbeSelector) {
        let mut open_devices = self.open_devices.lock().await;
        let wakers = open_devices.remove(probe).unwrap_or_default();
        for waker in wakers {
            waker.wake();
        }
    }
}

async fn server_info() -> Html<String> {
    let mut body = String::new();
    body.push_str("<!DOCTYPE html>");
    body.push_str("<html>");
    body.push_str("<head>");
    body.push_str("<title>probe-rs server info</title>");
    body.push_str("</head>");
    body.push_str("<body>");
    body.push_str("<h1>probe-rs status</h1>");

    let probes = Lister::new().list_all();
    if probes.is_empty() {
        body.push_str("<p>No probes connected</p>");
    } else {
        body.push_str("<ul>");
        for probe in probes {
            write!(body, "<li>{}</li>", probe).unwrap();
        }
    }

    body.push_str("</ul>");

    write!(body, "<p>Version: {}</p>", env!("PROBE_RS_LONG_VERSION")).unwrap();

    body.push_str("</body>");
    body.push_str("</html>");

    Html(body)
}

#[derive(clap::Parser, Serialize, Deserialize)]
pub struct Cmd {}

impl Cmd {
    pub async fn run(self, config: Config) -> anyhow::Result<()> {
        tracing::warn!("No users configured. Only accepting connections from localhost.");

        let state = Arc::new(ServerState::new(config));

        let app = Router::new()
            .route("/", get(server_info))
            .route("/worker", get(ws_handler))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();

        tracing::info!("listening on {}", listener.local_addr().unwrap());

        axum::serve(listener, app).await?;

        Ok(())
    }
}

pub struct ServerConnection {
    websocket: WebSocket,
    temp_files: Vec<NamedTempFile>,
}

impl ServerConnection {
    async fn send_message(&mut self, msg: ServerMessage) -> anyhow::Result<()> {
        let msg = serde_json::to_string(&msg).context("Failed to serialize message")?;
        self.websocket.send(ws::Message::Text(msg)).await?;

        Ok(())
    }

    pub async fn send_stdout(&mut self, msg: impl ToString) -> anyhow::Result<()> {
        let msg = ServerMessage::StdOut(msg.to_string());
        self.send_message(msg)
            .await
            .context("Failed to send stdout")
    }

    pub async fn send_stderr(&mut self, msg: impl ToString) -> anyhow::Result<()> {
        let msg = ServerMessage::StdErr(msg.to_string());
        self.send_message(msg)
            .await
            .context("Failed to send stderr")
    }

    async fn save_temp_file(&mut self, data: Vec<u8>) -> anyhow::Result<()> {
        let mut file = NamedTempFile::new().context("Failed to write temporary file")?;

        file.as_file_mut()
            .write_all(&data)
            .context("Failed to write temporary file")?;

        let path = file.path().to_path_buf();
        tracing::info!("Saved temporary file to {}", path.display());
        self.temp_files.push(file);

        let msg = ServerMessage::TempFileOpened(path);
        self.send_message(msg)
            .await
            .context("Failed to send file path")
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    state: State<Arc<ServerState>>,
    TypedHeader(auth): TypedHeader<headers::Authorization<authorization::Bearer>>,
) -> impl IntoResponse {
    // TODO: version check based on user agent
    let token = auth.0.token();
    let Some(user) = state.config.server_users.iter().find(|u| token == u.token) else {
        tracing::info!("Unknown token: {}", token);
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    };

    tracing::info!("User {} connected", user.name);

    // finalize the upgrade process by returning upgrade callback.
    // we can customize the callback by sending additional info such as address.
    ws.on_upgrade(move |socket| handle_socket(socket, state.0))
}

/// Actual websocket statemachine (one will be spawned per connection)
async fn handle_socket(socket: WebSocket, state: Arc<ServerState>) {
    let mut handle = ServerConnection {
        websocket: socket,
        temp_files: vec![],
    };
    while let Some(Ok(msg)) = handle.websocket.recv().await {
        if let ws::Message::Text(msg) = msg {
            let msg =
                serde_json::from_str::<ClientMessage>(&msg).expect("Failed to deserialize message");
            match msg {
                ClientMessage::TempFile(data) => handle.save_temp_file(data).await.unwrap(),
                ClientMessage::Command(cli) => {
                    if let Err(e) = run_command(&mut handle, cli, state.clone()).await {
                        handle.send_stderr(format!("{:?}", e)).await.unwrap();
                    }
                    handle.websocket.close().await.unwrap();
                    return;
                }
                ClientMessage::StdIn => todo!(),
            }
        }
    }
}

async fn run_command(
    handle: &mut ServerConnection,
    cli_string: String,
    state: Arc<ServerState>,
) -> anyhow::Result<()> {
    tracing::debug!("Running command: {}", cli_string);
    let cli = serde_json::from_str::<Cli>(&cli_string)?;
    let _guard = if let Some(probe) = cli.subcommand.probe_options().and_then(|o| o.probe.clone()) {
        state.device_opened(&probe).await;

        Some(AsyncDropper::new(RemoveDeviceGuard { state, probe }))
    } else {
        None
    };

    // Server starts a subprocess - this simplifies capturing stdout/stderr.
    let mut cmd = tokio::process::Command::new("probe-rs");
    cmd.args(["mi", "cli", &cli_string]);
    cmd.stdout(Stdio::piped());
    cmd.stdin(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let mut subprocess = cmd.spawn().context("Failed to spawn subprocess")?;

    let stdout = subprocess.stdout.take();
    let stderr = subprocess.stderr.take();

    let handle = Mutex::new(handle);

    let (result, _, _) = join3(
        subprocess.wait(),
        async {
            if let Some(stdout) = stdout {
                let mut reader = tokio::io::BufReader::new(stdout).lines();
                while let Some(line) = reader.next_line().await.unwrap() {
                    let mut handle = handle.lock().await;
                    handle.send_stdout(format!("{}\n", line)).await.unwrap();
                }
            }
        },
        async {
            if let Some(stderr) = stderr {
                let mut reader = tokio::io::BufReader::new(stderr).lines();
                while let Some(line) = reader.next_line().await.unwrap() {
                    let mut handle = handle.lock().await;
                    handle.send_stderr(format!("{}\n", line)).await.unwrap();
                }
            }
        },
    )
    .await;

    result?;

    Ok(())
}

struct RemoveDeviceGuard {
    state: Arc<ServerState>,
    probe: DebugProbeSelector,
}

impl AsyncDropEmulator for RemoveDeviceGuard {
    async fn drop(&mut self) {
        self.state.device_closed(&self.probe).await;
    }
}

// https://stackoverflow.com/questions/71541765/rust-async-drop

trait AsyncDropEmulator: Send + 'static {
    fn drop(&mut self) -> impl Future<Output = ()> + Send;
}

struct AsyncDropper<D: AsyncDropEmulator> {
    inner: Option<D>,
}

impl<D: AsyncDropEmulator> AsyncDropper<D> {
    fn new(inner: D) -> Self {
        Self { inner: Some(inner) }
    }
}

impl<D: AsyncDropEmulator> Drop for AsyncDropper<D> {
    fn drop(&mut self) {
        if let Some(mut inner) = self.inner.take() {
            tokio::spawn(async move { inner.drop().await });
        }
    }
}
