//! GDB server (local `Session` backend).
//!
//! `probe-rs gdb` uses [`crate::cmd::gdb_server_rpc`]. This module remains for
//! in-process callers such as `cargo-embed`.

pub(crate) mod arch;
mod stub;
pub(crate) mod target;

pub(crate) use stub::{GdbInstanceConfiguration, run};
