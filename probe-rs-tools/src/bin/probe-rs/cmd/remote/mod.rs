use std::path::PathBuf;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::cmd::remote::functions::{RemoteFunction, RemoteFunctions};

pub mod client;
pub mod functions;
pub mod server;

#[derive(Serialize, Deserialize)]
enum ClientMessage {
    TempFile(Vec<u8>),
    Command(String), // A serialized Cli
    StdIn,
    Rpc(RemoteFunctions),
}

#[derive(Debug, Serialize, Deserialize)]
enum ServerMessage {
    TempFileOpened(PathBuf),
    // PortOpened(u16),
    StdOut(String),
    StdErr(String),
    RpcResult(String),
}

pub trait SessionInterface {
    async fn run_call<F: RemoteFunction>(&mut self, func: F) -> anyhow::Result<F::Result>;
}

/// Run functions locally.
pub struct LocalSession {}

impl LocalSession {
    pub fn new() -> Self {
        Self {}
    }
}

impl SessionInterface for LocalSession {
    async fn run_call<F: RemoteFunction>(&mut self, func: F) -> anyhow::Result<F::Result> {
        Ok(func.run().await)
    }
}

/// Run functions on the remote server.
pub struct RemoteSession<'a> {
    client: &'a mut client::ClientConnection,
}

impl<'a> RemoteSession<'a> {
    pub fn new(client: &'a mut client::ClientConnection) -> Self {
        Self { client }
    }
}

impl SessionInterface for RemoteSession<'_> {
    async fn run_call<F: RemoteFunction>(&mut self, func: F) -> anyhow::Result<F::Result> {
        let response = self.client.run_call(func.into()).await?;
        let response = serde_json::from_str::<F::Result>(&response)
            .context("Failed to deserialize response")?;
        Ok(response)
    }
}
