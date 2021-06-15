//! Host side implementation of the RTT (Real-Time Transfer) I/O protocol over probe-rs
//!
//! RTT implements input and output to/from a microcontroller using in-memory ring buffers and
//! memory polling. This enables debug logging from the microcontroller with minimal delays and no
//! blocking, making it usable even in real-time applications where e.g. semihosting delays cannot
//! be tolerated.
//!
//! This crate enables you to read and write via RTT channels. It's also used as a building-block
//! for probe-rs debugging tools.
//!
//! ## Example
//!
//! ```no_run
//! use std::sync::{Arc, Mutex};
//! use probe_rs::Probe;
//! use probe_rs_rtt::Rtt;
//!
//! // First obtain a probe-rs session (see probe-rs documentation for details)
//! let probe = Probe::list_all()[0].open()?;
//! let mut session = probe.attach("somechip")?;
//!
//! // Attach to RTT
//! let mut rtt = Rtt::attach(Arc::new(Mutex::new(session)))?;
//!
//! // Read from a channel
//! if let Some(input) = rtt.up_channels().take(0) {
//!     let mut buf = [0u8; 1024];
//!     let count = input.read(&mut buf[..])?;
//!
//!     println!("Read data: {:?}", &buf[..count]);
//! }
//!
//! // Write to a channel
//! if let Some(output) = rtt.down_channels().take(0) {
//!     output.write(b"Hello, computer!\n")?;
//! }
//!
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use thiserror::Error;

mod channel;
pub use channel::*;

pub mod channels;
pub use channels::Channels;

mod rtt;
pub use rtt::*;

/// Error type for RTT operations.
#[derive(Error, Debug)]
pub enum Error {
    /// RTT control block not found in target memory. Make sure RTT is initialized on the target.
    #[error(
        "RTT control block not found in target memory.\n\
        - Make sure RTT is initialized on the target.\n\
        - Depending on the target, sleep modes can interfere with RTT."
    )]
    ControlBlockNotFound,

    /// Multiple control blocks found in target memory. The data contains the control block addresses (up to 5).
    #[error("Multiple control blocks found in target memory.")]
    MultipleControlBlocksFound(Vec<u32>),

    /// The control block has been corrupted. The data contains a detailed error.
    #[error("Control block corrupted: {0}")]
    ControlBlockCorrupted(String),

    /// Wraps errors propagated up from probe-rs.
    #[error("Error communicating with probe: {0}")]
    Probe(#[from] probe_rs::Error),
}
