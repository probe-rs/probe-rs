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
//! use probe_rs::{Probe, Permissions};
//! use probe_rs_rtt::Rtt;
//!
//! // First obtain a probe-rs session (see probe-rs documentation for details)
//! let probe = Probe::list_all()[0].open()?;
//! let mut session = probe.attach("somechip", Permissions::default())?;
//! let memory_map = session.target().memory_map.clone();
//! // Select a core.
//! let mut core = session.core(0)?;
//!
//! // Attach to RTT
//! let mut rtt = Rtt::attach(&mut core, &memory_map)?;
//!
//! // Read from a channel
//! if let Some(input) = rtt.up_channels().take(0) {
//!     let mut buf = [0u8; 1024];
//!     let count = input.read(&mut core, &mut buf[..])?;
//!
//!     println!("Read data: {:?}", &buf[..count]);
//! }
//!
//! // Write to a channel
//! if let Some(output) = rtt.down_channels().take(0) {
//!     output.write(&mut core, b"Hello, computer!\n")?;
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
        - Make sure RTT is initialized on the target, AND that there are NO target breakpoints before RTT initalization.\n\
        - For VSCode and probe-rs-debugger users, using `halt_after_reset:true` in your `launch.json` file will prevent RTT \n\
        \tinitialization from happening on time.\n\
        - Depending on the target, sleep modes can interfere with RTT."
    )]
    ControlBlockNotFound,

    /// Multiple control blocks found in target memory. The data contains the control block addresses (up to 5).
    #[error("Multiple control blocks found in target memory.")]
    MultipleControlBlocksFound(Vec<u32>),

    /// The control block has been corrupted. The data contains a detailed error.
    #[error("Control block corrupted: {0}")]
    ControlBlockCorrupted(String),

    /// Attempted an RTT read/write operation against a Core number that is different from the Core number against which RTT was initialized
    #[error("Incorrect Core number specified for this operation. Expected {0}, and found {1}")]
    IncorrectCoreSpecified(usize, usize),

    /// Wraps errors propagated up from probe-rs.
    #[error("Error communicating with probe: {0}")]
    Probe(#[from] probe_rs::Error),

    /// Wraps errors propagated up from reading memory on the target.
    #[error("Unexpected error while reading {0} from target memory. Please report this as a bug.")]
    MemoryRead(String),
}
