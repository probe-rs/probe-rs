//! The host side of semihosting runs on the client. The server sends each call that needs a host
//! resource to the client, and the client replies with a value.

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

/// The maximum number of bytes that one `Read` or `Write` moves.
pub const MAX_CHUNK: u32 = 64 * 1024;

/// The maximum length of a path.
pub const MAX_PATH: u32 = 4 * 1024;

/// The maximum number of open files and sockets in one monitor or test call.
pub const MAX_OPEN_FILES: usize = 64;

/// A semihosting open mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub enum OpenMode {
    /// `r`
    Read,
    /// `r+`
    ReadUpdate,
    /// `w`
    Write,
    /// `w+`
    WriteUpdate,
    /// `a`
    Append,
    /// `a+`
    AppendUpdate,
}

/// What an open handle is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub enum FileKind {
    File,
    Stream,
}

/// A call from the server to the client.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct HostCall {
    /// Increases for each call of a connection.
    pub id: u64,
    pub request: HostRequest,
}

#[derive(Debug, Serialize, Deserialize, Schema)]
pub enum HostRequest {
    Open { path: String, mode: OpenMode },
    Close { handle: u32 },
    Read { handle: u32, len: u32 },
    Write { handle: u32, bytes: Vec<u8> },
    Seek { handle: u32, pos: u32 },
    FileLength { handle: u32 },
}

/// The reply of the client to a [`HostCall`].
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct HostReply {
    pub id: u64,
    pub result: Result<HostValue, HostError>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub enum HostValue {
    Opened { handle: u32, kind: FileKind },
    Done,
    Read(Vec<u8>),
    Written(u32),
    Length(u64),
}

/// A portable error. It has no text, so that no host path or OS message reaches the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub enum HostError {
    NotMapped,
    BadHandle,
    TooManyFiles,
    NotFound,
    PermissionDenied,
    AlreadyExists,
    IsADirectory,
    InvalidInput,
    Interrupted,
    Other,
}

impl HostError {
    /// Returns the value for `SYS_ERRNO`. These are fixed values, because the numbers of the host
    /// differ between systems.
    pub fn errno(self) -> i32 {
        match self {
            Self::NotFound => 2,
            Self::Interrupted => 4,
            Self::Other => 5,
            Self::BadHandle => 9,
            Self::NotMapped | Self::PermissionDenied => 13,
            Self::AlreadyExists => 17,
            Self::IsADirectory => 21,
            Self::InvalidInput => 22,
            Self::TooManyFiles => 24,
        }
    }
}

impl From<std::io::Error> for HostError {
    fn from(error: std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound,
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            std::io::ErrorKind::AlreadyExists => Self::AlreadyExists,
            std::io::ErrorKind::IsADirectory => Self::IsADirectory,
            std::io::ErrorKind::InvalidInput => Self::InvalidInput,
            std::io::ErrorKind::Interrupted => Self::Interrupted,
            _ => Self::Other,
        }
    }
}
