//! Postcard RPC wire transport implementation, using `futures_util::Sink` and
//! `futures_util::Stream` types as the underlying transport, and 4-byte length prefix encoding.

pub mod memory;
#[cfg(feature = "remote")]
pub mod websocket;
