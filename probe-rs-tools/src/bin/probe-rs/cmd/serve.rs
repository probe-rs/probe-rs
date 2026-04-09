//! Remote server
//!
//! The server listens for incoming websocket connections and executes commands on behalf of the
//! client. The server also provides a status webpage that shows the available probes.

use axum::{
    Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{self, WebSocket},
    },
    http::HeaderValue,
    response::{Html, IntoResponse},
    routing::{any, get},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use postcard_rpc::server::WireRxErrorKind;
use probe_rs::probe::list::Lister;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use tokio::task::LocalSet;
use tokio_util::bytes::Bytes;

use std::{fmt::Write, path::PathBuf, sync::Arc};

use crate::rpc::{
    functions::{ProbeAccess, RpcApp},
    transport::websocket::{AxumWebsocketTx, WebsocketRx},
};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerConfig {
    pub users: Vec<ServerUser>,
    pub address: Option<String>,
    pub port: Option<u16>,
}

impl ServerConfig {
    #[cfg(unix)]
    pub fn socket_path(&self) -> Option<&str> {
        self.address
            .as_ref()
            .and_then(|addr| addr.strip_prefix("socket://"))
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        #[cfg(unix)]
        if self.socket_path().is_some() && self.port.is_some() {
            tracing::warn!("Port has no meaning for a Unix socket, it will be ignored.");
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerUser {
    pub name: String,
    pub token: String,
    #[serde(default)]
    pub access: ProbeAccess,
}

struct ServerState {
    config: ServerConfig,
    requests: tokio::sync::mpsc::Sender<(WebSocket, String)>,
}

impl ServerState {
    fn new(config: ServerConfig, requests: tokio::sync::mpsc::Sender<(WebSocket, String)>) -> Self {
        Self { config, requests }
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
            write!(body, "<li>{probe}</li>").unwrap();
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
    pub async fn run(self, config: ServerConfig) -> anyhow::Result<()> {
        config.validate()?;

        if config.users.is_empty() {
            tracing::warn!("No users configured.");
        }

        #[cfg(unix)]
        if let Some(socket_path) = config.socket_path() {
            return self.run_unix(&PathBuf::from(socket_path), config).await;
        }

        self.run_tcp(config).await
    }

    async fn run_tcp(self, config: ServerConfig) -> anyhow::Result<()> {
        let address = config.address.as_deref().unwrap_or("0.0.0.0");
        let port = config.port.unwrap_or(3000);

        let listener = tokio::net::TcpListener::bind(format!("{address}:{port}"))
            .await
            .unwrap();

        let (request_tx, mut request_rx) = tokio::sync::mpsc::channel(64);

        let set = LocalSet::new();
        let state = Arc::new(ServerState::new(config, request_tx));

        set.spawn_local({
            let state = state.clone();
            async move {
                while let Some((socket, challenge)) = request_rx.recv().await {
                    // Spawn a new task for each connection
                    tokio::task::spawn_local(handle_socket(socket, challenge, state.clone()));
                }
            }
        });

        let app = Router::new()
            .route("/", get(server_info))
            .route("/worker", any(ws_handler))
            .with_state(state);

        tracing::info!("listening on {}", listener.local_addr().unwrap());

        let (result, _) = tokio::join! {
            axum::serve(listener, app),
            set,
        };

        result.unwrap();

        Ok(())
    }

    #[cfg(unix)]
    async fn run_unix(self, socket_path: &PathBuf, _config: ServerConfig) -> anyhow::Result<()> {
        use std::fs::{metadata, set_permissions};
        use std::os::unix::fs::PermissionsExt;
        use tokio::net::UnixListener;

        if socket_path.exists() {
            tracing::info!("removing existing unix socket for server");
            std::fs::remove_file(socket_path)?;
        }

        let listener = UnixListener::bind(socket_path)?;

        let mut perms = metadata(socket_path)?.permissions();
        perms.set_mode(0o660);
        set_permissions(socket_path, perms)?;

        tracing::info!("listening on {}", socket_path.display());

        loop {
            let (stream, _) = listener.accept().await?;

            // Spawn a new task for each connection
            tokio::spawn(handle_unix_rpc(stream));
        }
    }
}

async fn ws_handler(ws: WebSocketUpgrade, state: State<Arc<ServerState>>) -> impl IntoResponse {
    // Generate a random challenge that the client needs to hash together with their token.
    // We don't need a cryptographically secure random number here, just something to make
    // replays harder.
    let mut challenge = [0; 64];
    fastrand::fill(&mut challenge);
    let challenge = STANDARD.encode(challenge);

    // finalize the upgrade process by returning upgrade callback.
    // we can customize the callback by sending additional info such as address.
    let mut response = ws.on_upgrade({
        let challenge = challenge.clone();
        async move |socket| {
            // Send the request out of here so the task can be spawned on the local set.
            state.requests.send((socket, challenge)).await.unwrap();
        }
    });

    response.headers_mut().insert(
        "Probe-Rs-Challenge",
        HeaderValue::from_str(challenge.as_str()).unwrap(),
    );

    response
}

static SERVER_DEPTH: usize = 16;

/// Actual websocket state machine (one will be spawned per connection on the local set)
async fn handle_socket(socket: WebSocket, challenge: String, state: Arc<ServerState>) {
    let (writer, reader) = socket.split();

    let mut reader = WebsocketRx::new(reader.map(|message| {
        message.map(|message| match message {
            ws::Message::Binary(binary) => binary,
            _ => Bytes::new(),
        })
    }));

    let Some(Ok(challenge_response)) = reader.next().await else {
        tracing::warn!("Client disconnected before sending challenge response");
        return;
    };

    // TODO: we might want to include the username to avoid hashing a bunch of times
    let mut authed_user = None;
    for user in state.config.users.iter() {
        let mut hasher = Sha512::new();
        hasher.update(challenge.as_bytes());
        hasher.update(user.token.as_bytes());
        let result = hasher.finalize().to_vec();

        if challenge_response == result {
            authed_user = Some(user);
            break;
        }
    }

    let Some(user) = authed_user else {
        tracing::warn!("Client failed to authenticate");
        return;
    };

    tracing::info!("User {} connected", user.name);

    let (mut server, tx, mut rx) = RpcApp::create_server(SERVER_DEPTH, user.access.clone());

    // Connect the server's channels to the websocket connection
    let sender = async {
        let mut writer = AxumWebsocketTx::new(writer);
        // Send messages from the server to the client.
        while let Some(msg) = rx.recv().await {
            writer.send(msg).await.unwrap();
        }
    };

    let receiver = async {
        // Forward messages from the client to the server
        while let Some(msg) = reader.next().await {
            tx.send(msg.map_err(|_| WireRxErrorKind::Other))
                .await
                .unwrap();
        }
    };

    tokio::select! {
        _ = server.run() => tracing::warn!("Server stopped"),
        _ = sender => tracing::warn!("Server sender stopped"),
        _ = receiver => tracing::info!("Client disconnected"),
    }
}

#[cfg(unix)]
async fn handle_unix_rpc(stream: tokio::net::UnixStream) {
    use crate::rpc::transport::memory::{PostcardReceiver, PostcardSender};
    use crate::rpc::transport::unix::{UnixStreamRx, UnixStreamTx};

    tracing::info!("Unix socket client connected");

    let (reader, writer) = stream.into_split();
    let (mut server, tx, mut rx) = RpcApp::create_server(SERVER_DEPTH, ProbeAccess::All);

    // Connect the server's channels to the unix socket connection
    let sender = async {
        let writer = UnixStreamTx::new(writer);

        // Send messages from the server to the client.
        while let Some(msg) = rx.recv().await {
            if writer.send(msg).await.is_err() {
                tracing::error!("Failed to send msg to unix socket, terminating sender loop.");
                break;
            }
        }
    };

    let receiver = async {
        let mut reader = UnixStreamRx::new(reader);

        // Forward messages from the client to the server.
        loop {
            let msg = reader.receive().await;
            if tx.send(msg).await.is_err() {
                tracing::error!(
                    "Failed to forward msg from unix socket, terminating receiver loop."
                );
                break;
            }
        }
    };

    tokio::select! {
        _ = server.run() => tracing::warn!("Server stopped"),
        _ = sender => tracing::warn!("Server sender stopped"),
        _ = receiver => tracing::info!("Client disconnected"),
    }
}
