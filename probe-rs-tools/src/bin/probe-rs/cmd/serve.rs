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
use futures_util::{
    Sink, SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use postcard_rpc::server::WireRxErrorKind;
use probe_rs::probe::list::Lister;
use probe_rs_rpc::transport::{frame, websocket::WebsocketRx};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use tokio::task::LocalSet;
use tokio_util::bytes::Bytes;

#[cfg(unix)]
use std::path::PathBuf;
use std::{fmt::Write, sync::Arc, time::Duration};
use std::{
    pin::Pin,
    task::{Context, Poll},
};

use crate::{
    rpc::functions::{ProbeAccess, RpcApp},
    rpc::probe_broker::ProbeBroker,
    util::{probe_config::ProbeConfigs, pwr},
};

// Sends length-prefixed binary messages to a websocket stream
pub struct AxumWebsocketTx<S> {
    writer: S,
}
impl<S> AxumWebsocketTx<S> {
    pub fn new(writer: S) -> Self {
        Self { writer }
    }
}

impl<S> Sink<Vec<u8>> for AxumWebsocketTx<S>
where
    S: Sink<ws::Message> + Unpin,
{
    type Error = S::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.writer.poll_ready_unpin(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, msg: Vec<u8>) -> Result<(), Self::Error> {
        self.writer
            .start_send_unpin(ws::Message::Binary(frame(&msg).freeze()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.writer.poll_flush_unpin(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.writer.poll_close_unpin(cx)
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerConfig {
    pub users: Vec<ServerUser>,
    pub address: Option<String>,
    pub port: Option<u16>,
    pub cycle_power: bool,
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
    probe_broker: Arc<ProbeBroker>,
}

impl ServerState {
    fn new(
        config: ServerConfig,
        requests: tokio::sync::mpsc::Sender<(WebSocket, String)>,
        probe_broker: Arc<ProbeBroker>,
    ) -> Self {
        Self {
            config,
            requests,
            probe_broker,
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
            write!(body, "<li>{probe}</li>").unwrap();
        }
        body.push_str("</ul>");
    }

    write!(body, "<p>Version: {}</p>", env!("PROBE_RS_LONG_VERSION")).unwrap();

    body.push_str("</body>");
    body.push_str("</html>");

    Html(body)
}

#[derive(clap::Parser, Serialize, Deserialize)]
pub struct Cmd {}

impl Cmd {
    pub async fn run(
        self,
        config: ServerConfig,
        probe_configs: ProbeConfigs,
    ) -> anyhow::Result<()> {
        config.validate()?;

        if config.users.is_empty() {
            tracing::warn!("No users configured.");
        }

        if config.cycle_power {
            pwr::power_enable().await?;
        }

        let probe_broker = Arc::new(ProbeBroker::new_with_config(probe_configs));

        #[cfg(unix)]
        if let Some(socket_path) = config.socket_path() {
            return self
                .run_unix(&PathBuf::from(socket_path), config, probe_broker)
                .await;
        }

        self.run_tcp(config, probe_broker).await
    }

    async fn run_tcp(
        self,
        config: ServerConfig,
        probe_broker: Arc<ProbeBroker>,
    ) -> anyhow::Result<()> {
        let address = config.address.as_deref().unwrap_or("0.0.0.0");
        let port = config.port.unwrap_or(3000);

        let listener = tokio::net::TcpListener::bind(format!("{address}:{port}"))
            .await
            .unwrap();

        tracing::info!("listening on {}", listener.local_addr().unwrap());

        serve_websockets(listener, config, probe_broker, CLIENT_TIMEOUT).await;

        Ok(())
    }

    #[cfg(unix)]
    async fn run_unix(
        self,
        socket_path: &PathBuf,
        _config: ServerConfig,
        probe_broker: Arc<ProbeBroker>,
    ) -> anyhow::Result<()> {
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
            tokio::spawn(handle_unix_rpc(stream, probe_broker.clone()));
        }
    }
}

/// Serves the status page and the websocket endpoint on `listener`.
async fn serve_websockets(
    listener: tokio::net::TcpListener,
    config: ServerConfig,
    probe_broker: Arc<ProbeBroker>,
    client_timeout: Duration,
) {
    let (request_tx, mut request_rx) = tokio::sync::mpsc::channel(64);

    let set = LocalSet::new();
    let state = Arc::new(ServerState::new(config, request_tx, probe_broker));

    set.spawn_local({
        let state = state.clone();
        async move {
            while let Some((socket, challenge)) = request_rx.recv().await {
                // Spawn a new task for each connection
                tokio::task::spawn_local(handle_socket(
                    socket,
                    challenge,
                    state.clone(),
                    client_timeout,
                ));
            }
        }
    });

    let app = Router::new()
        .route("/", get(server_info))
        .route("/worker", any(ws_handler))
        .with_state(state);

    let (result, _) = tokio::join! {
        axum::serve(listener, app),
        set,
    };

    result.unwrap();
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

/// The server drops a client that does not answer a ping, or does not read a message, in this
/// time. It pings the client, and checks for an answer, every third of this time.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(30);

/// Actual websocket state machine (one will be spawned per connection on the local set)
async fn handle_socket(
    socket: WebSocket,
    challenge: String,
    state: Arc<ServerState>,
    client_timeout: Duration,
) {
    let (mut writer, reader) = socket.split();

    // Send a close frame, so that the client can tell a clean exit from a lost connection.
    if serve_socket(&mut writer, reader, challenge, state, client_timeout).await {
        _ = tokio::time::timeout(client_timeout, writer.close()).await;
    }
}

/// Returns `false` if the client stopped reading.
async fn serve_socket(
    writer: &mut SplitSink<WebSocket, ws::Message>,
    reader: SplitStream<WebSocket>,
    challenge: String,
    state: Arc<ServerState>,
    client_timeout: Duration,
) -> bool {
    // The time of the first ping after the last frame from the client. Any frame is an answer.
    let unanswered_ping = std::cell::Cell::new(None);
    let mut reader = WebsocketRx::new(reader.map(|message| {
        unanswered_ping.set(None);
        message.map(|message| match message {
            ws::Message::Binary(binary) => binary,
            _ => Bytes::new(),
        })
    }));

    let challenge_response = match tokio::time::timeout(client_timeout, reader.next()).await {
        Ok(Some(Ok(challenge_response))) => challenge_response,
        Ok(_) => {
            tracing::warn!("Client disconnected before sending challenge response");
            return true;
        }
        Err(_) => {
            tracing::warn!("Client did not send challenge response in time");
            return true;
        }
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
        return true;
    };

    tracing::info!("User {} connected", user.name);

    let (server, tx, mut rx) = RpcApp::create_server(
        SERVER_DEPTH,
        user.access.clone(),
        state.probe_broker.clone(),
    );

    // Connect the server's channels to the websocket connection
    let sender = async {
        let mut writer = AxumWebsocketTx::new(writer);
        let period = client_timeout / 3;
        let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Send messages from the server to the client.
        loop {
            let sent = tokio::select! {
                msg = rx.recv() => {
                    let Some(msg) = msg else { break };
                    tokio::time::timeout(client_timeout, writer.send(msg)).await
                }
                _ = ping.tick() => {
                    let ping = writer.writer.send(ws::Message::Ping(Bytes::new()));
                    let sent = tokio::time::timeout(client_timeout, ping).await;
                    if matches!(sent, Ok(Ok(()))) && unanswered_ping.get().is_none() {
                        unanswered_ping.set(Some(tokio::time::Instant::now()));
                    }
                    sent
                }
            };
            match sent {
                Ok(Ok(())) => {}
                Ok(Err(_)) => break,
                Err(_) => {
                    tracing::warn!("Client stopped reading");
                    return false;
                }
            }
        }
        tracing::warn!("Server sender stopped");
        true
    };

    let receiver = async {
        let mut check = tokio::time::interval(client_timeout / 3);

        // Forward messages from the client to the server
        loop {
            // Read first, so that answers that came in while the server was busy count.
            let msg = tokio::select! {
                biased;

                msg = reader.next() => msg,
                _ = check.tick() => {
                    let sent = unanswered_ping.get();
                    if sent.is_some_and(|sent| sent.elapsed() >= client_timeout) {
                        tracing::warn!("Client stopped responding");
                        break;
                    }
                    continue;
                }
            };
            let Some(msg) = msg else {
                tracing::info!("Client disconnected");
                break;
            };
            if tx
                .send(msg.map_err(|_| WireRxErrorKind::Other))
                .await
                .is_err()
            {
                tracing::warn!("Server stopped");
                break;
            }
        }
    };

    tokio::select! {
        _ = server.run() => {
            tracing::warn!("Server stopped");
            true
        }
        reads = sender => reads,
        _ = receiver => true,
    }
}

#[cfg(unix)]
async fn handle_unix_rpc(stream: tokio::net::UnixStream, probe_broker: Arc<ProbeBroker>) {
    use probe_rs_rpc::transport::memory::{PostcardReceiver, PostcardSender};
    use probe_rs_rpc::transport::unix::{UnixStreamRx, UnixStreamTx};

    tracing::info!("Unix socket client connected");

    let (reader, writer) = stream.into_split();
    let (server, tx, mut rx) = RpcApp::create_server(SERVER_DEPTH, ProbeAccess::All, probe_broker);

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use tokio_tungstenite::tungstenite::Message;

    const TIMEOUT: Duration = Duration::from_secs(1);

    /// The server must end the websocket with a close frame, so that the
    /// client can tell a refused login from a lost connection.
    #[tokio::test]
    async fn a_refused_login_closes_the_websocket() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        // No users, so every login fails.
        let server = serve_websockets(
            listener,
            ServerConfig::default(),
            Arc::new(ProbeBroker::new()),
            TIMEOUT,
        );

        let client = async {
            let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{address}/worker"))
                .await
                .unwrap();
            ws.send(Message::Binary(frame(b"wrong").freeze()))
                .await
                .unwrap();

            ws.collect::<Vec<_>>().await
        };

        let messages = tokio::select! {
            _ = server => panic!("the server stopped"),
            messages = tokio::time::timeout(Duration::from_secs(5), client) => {
                messages.expect("the websocket stayed open")
            }
        };

        assert!(
            messages
                .iter()
                .any(|message| matches!(message, Ok(Message::Close(_)))),
            "no close frame, the client got: {messages:?}"
        );
    }

    type ClientWebsocket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    const TOKEN: &str = "token";

    /// Runs a server with one user on `listener`.
    async fn serve(listener: tokio::net::TcpListener) {
        let config = ServerConfig {
            users: vec![ServerUser {
                name: "user".to_owned(),
                token: TOKEN.to_owned(),
                access: ProbeAccess::All,
            }],
            ..ServerConfig::default()
        };
        serve_websockets(listener, config, Arc::new(ProbeBroker::new()), TIMEOUT).await
    }

    async fn log_in(address: std::net::SocketAddr) -> ClientWebsocket {
        let (mut ws, response) = tokio_tungstenite::connect_async(format!("ws://{address}/worker"))
            .await
            .unwrap();
        let challenge = response.headers()["Probe-Rs-Challenge"].to_str().unwrap();
        let mut hasher = Sha512::new();
        hasher.update(challenge.as_bytes());
        hasher.update(TOKEN.as_bytes());
        ws.send(Message::Binary(frame(&hasher.finalize()).freeze()))
            .await
            .unwrap();

        // The server only pings a client that logged in.
        loop {
            match ws.next().await {
                Some(Ok(Message::Ping(_))) => return ws,
                Some(Ok(_)) => {}
                _ => panic!("the login failed"),
            }
        }
    }

    /// Runs a server, logs in, and runs `client` on the websocket.
    async fn with_logged_in_client<R>(client: impl AsyncFnOnce(ClientWebsocket) -> R) -> R {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        tokio::select! {
            _ = serve(listener) => panic!("the server stopped"),
            result = async { client(log_in(address).await).await } => result,
        }
    }

    /// The server must end the connection of a client that stopped
    /// responding, for example after a power loss. Otherwise its requests run
    /// on and hold the probes.
    #[tokio::test]
    async fn the_server_drops_a_client_that_stopped_responding() {
        let ended = with_logged_in_client(async |mut ws| {
            // Stop responding. Reading the websocket would answer the pings of the server.
            tokio::time::sleep(TIMEOUT * 2).await;

            tokio::time::timeout(Duration::from_secs(5), async {
                while ws.next().await.is_some() {}
            })
            .await
        })
        .await;

        assert!(ended.is_ok(), "the server kept the connection open");
    }

    /// An idle client answers the pings of the server, and stays connected.
    #[tokio::test]
    async fn the_server_keeps_an_idle_client() {
        let ended = with_logged_in_client(async |mut ws| {
            tokio::time::timeout(TIMEOUT * 2, async { while ws.next().await.is_some() {} }).await
        })
        .await;

        assert!(ended.is_err(), "the server dropped an idle client");
    }

    /// Runs a server, and blocks its thread for `stall` right after its second ping. A client on
    /// another thread answers this ping after `pong_delay`, and the other pings at once. Returns
    /// when the pings after the first arrived, and if the server ended the connection.
    async fn stall_the_server(stall: Duration, pong_delay: Duration) -> (Vec<Instant>, bool) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (pinged_tx, pinged_rx) = tokio::sync::oneshot::channel();

        let client = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let mut ws = log_in(address).await;
                let mut pings = vec![];
                let mut pinged_tx = Some(pinged_tx);
                let ended = tokio::time::timeout(stall + TIMEOUT, async {
                    while let Some(Ok(message)) = ws.next().await {
                        if let Message::Ping(_) = message {
                            if pings.is_empty() {
                                _ = pinged_tx.take().unwrap().send(());
                                // The client answers when it reads or writes again.
                                tokio::time::sleep(pong_delay).await;
                            }
                            pings.push(Instant::now());
                        }
                    }
                })
                .await;
                (pings, ended.is_ok())
            })
        });

        let stall_server = async {
            pinged_rx.await.unwrap();
            std::thread::sleep(stall);
            tokio::task::spawn_blocking(move || client.join().unwrap())
                .await
                .unwrap()
        };

        tokio::select! {
            _ = serve(listener) => panic!("the server stopped"),
            result = stall_server => result,
        }
    }

    /// A long blocking call stops the server thread. The server must not drop a client that
    /// answered a ping in this time.
    #[tokio::test]
    async fn the_server_keeps_a_client_after_a_stall() {
        let (_, ended) = stall_the_server(TIMEOUT * 2, TIMEOUT / 5).await;

        assert!(
            !ended,
            "the server dropped a client that answered the pings"
        );
    }

    /// The server must not send the pings that it missed in a stall back to back.
    #[tokio::test]
    async fn the_server_sends_no_burst_of_pings_after_a_stall() {
        let (pings, ended) = stall_the_server(TIMEOUT * 5 / 6, Duration::ZERO).await;

        assert!(!ended, "the server dropped the client");
        assert!(pings.len() >= 2, "only {} pings came", pings.len());
        // A burst sends the pings microseconds apart.
        let gap = pings.windows(2).map(|w| w[1] - w[0]).min().unwrap();
        assert!(
            gap > Duration::from_millis(10),
            "two pings came {gap:?} apart"
        );
    }
}
