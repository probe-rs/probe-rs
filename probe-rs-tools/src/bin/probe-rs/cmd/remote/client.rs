//! Remote client
//!
//! The client opens a websocket connection to the host, sends a token to authenticate and
//! then sends commands to the server. The commands may upload temporary files to the server,
//! which are then used by the server to execute the command.

use anyhow::Context as _;
use axum::http::Uri;
use futures_util::{SinkExt, StreamExt as _};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{ClientRequestBuilder, Message},
    MaybeTlsStream, WebSocketStream,
};

use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use super::{ClientMessage, ServerMessage};
use crate::{cmd::remote::functions::RemoteFunctions, Cli};

/// Represents a connection to a remote server.
///
/// Internally implemented as a websocket connection.
pub struct ClientConnection {
    websocket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    is_localhost: bool,
}

impl ClientConnection {
    pub async fn send_command(&mut self, mut cli: Cli) -> anyhow::Result<()> {
        // We don't want to recusively call the server
        cli.token = None;
        cli.host = None;

        let cli_json = serde_json::to_string(&cli).context("Failed to serialize CLI command")?;

        let msg = ClientMessage::Command(cli_json);
        let msg = serde_json::to_string(&msg).context("Failed to serialize client message")?;

        self.websocket.send(Message::Text(msg.into())).await?;

        while let Some(Ok(msg)) = self.websocket.next().await {
            match msg {
                Message::Text(msg) => {
                    let msg = serde_json::from_str::<ServerMessage>(&msg)
                        .context("Failed to parse server message")?;
                    match msg {
                        ServerMessage::StdOut(msg) => print!("{}", msg),
                        ServerMessage::StdErr(msg) => eprint!("{}", msg),
                        msg => panic!("Command unexpectedly returned {msg:?}"),
                    }
                }
                Message::Close(_) => break,
                _ => (),
            }
        }

        Ok(())
    }

    pub async fn upload_file(&mut self, path: impl AsRef<Path>) -> anyhow::Result<PathBuf> {
        let path = path.as_ref();
        if self.is_localhost {
            return Ok(path.to_path_buf());
        }

        let data = tokio::fs::read(path).await.context("Failed to read file")?;
        let msg = ClientMessage::TempFile(data);
        let msg = serde_json::to_string(&msg).context("Failed to serialize client message")?;
        self.websocket.send(Message::Text(msg.into())).await?;

        if let Some(Ok(Message::Text(msg))) = self.websocket.next().await {
            let msg = serde_json::from_str::<ServerMessage>(&msg)
                .context("Failed to parse server message")?;
            match msg {
                ServerMessage::TempFileOpened(path) => return Ok(path),
                msg => panic!("Command unexpectedly returned {msg:?}"),
            }
        }

        anyhow::bail!("Server did not return a file path")
    }

    pub async fn run_call(&mut self, f: RemoteFunctions) -> anyhow::Result<String> {
        let msg = ClientMessage::Rpc(f);
        let msg = serde_json::to_string(&msg).context("Failed to serialize client message")?;

        self.websocket.send(Message::Text(msg.into())).await?;

        if let Some(Ok(msg)) = self.websocket.next().await {
            match msg {
                Message::Text(msg) => {
                    let msg = serde_json::from_str::<ServerMessage>(&msg)
                        .context("Failed to parse server message")?;
                    match msg {
                        ServerMessage::RpcResult(msg) => return Ok(msg),
                        ServerMessage::Error(msg) => anyhow::bail!("{msg}"),
                        msg => panic!("Command unexpectedly returned {msg:?}"),
                    }
                }
                Message::Close(_) => {}
                msg => panic!("Server unexpectedly sent {msg:?}"),
            }
        }

        anyhow::bail!("Connection closed unexpectedly")
    }
}

pub async fn connect(host: &str, token: Option<String>) -> anyhow::Result<ClientConnection> {
    let uri = Uri::from_str(&format!("{}/worker", host)).context("Failed to parse server URI")?;

    let is_localhost = uri
        .host()
        .map_or(false, |h| ["localhost", "127.0.0.1", "::1"].contains(&h));
    let req = ClientRequestBuilder::new(uri).with_header(
        "User-Agent",
        format!("probe-rs-tools {}", env!("PROBE_RS_LONG_VERSION")),
    );

    let req = if let Some(token) = token {
        req.with_header("Authorization", format!("Bearer {token}"))
    } else {
        req
    };

    let (ws_stream, _) = connect_async(req).await.context("Failed to connect")?;

    Ok(ClientConnection {
        websocket: ws_stream,
        is_localhost,
    })
}
