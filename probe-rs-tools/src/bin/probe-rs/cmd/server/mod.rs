use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod client;
pub mod server;

#[derive(Serialize, Deserialize)]
enum ClientMessage {
    TempFile(Vec<u8>),
    Command(String), // A serialized Cli
    StdIn,
}

#[derive(Debug, Serialize, Deserialize)]
enum ServerMessage {
    TempFileOpened(PathBuf),
    // PortOpened(u16),
    StdOut(String),
    StdErr(String),
}
