//! # As short as it gets 
//! ```
//! # use probe_rs::DebugProbeError;
//! use probe_rs::Probe;
//!
//! // Get a list of all available debug probes.
//! let probes = Probe::list_all();
//!
//! // Use the first probe found.
//! let probe = probes[0].open()?;
//!
//! // Attach to a chip.
//! let session = probe.attach("nrf52")?;
//! 
//! // Select a core.
//! let core = session.attach_to_core(0)?;
//! 
//! // Halt the attached core.
//! core.halt()?; 
//! # Ok::<(), DebugProbeError>(())
//! ```
//!
//! probe-rs is built around 5 main interfaces: the [Probe](./struct.Probe.html),
//! [Target](./struct.Target.html), [Session](./struct.Session.html), [Memory](./struct.Memory.html) and [Core](./struct.Core.html) strucs.

#![allow(clippy::useless_let_if_seq)]
#![allow(clippy::trivially_copy_pass_by_ref)]
#![allow(clippy::implicit_hasher)]
#![allow(clippy::verbose_bit_mask)]

#[macro_use]
extern crate derivative;
#[macro_use]
extern crate maplit;
#[macro_use]
extern crate serde_derive;

pub mod architecture;
pub mod config;
mod core;
pub mod debug;
mod error;
pub mod flash;
mod memory;
mod probe;
mod session;

pub use crate::config::target::Target;
pub use crate::core::{
    Breakpoint, BreakpointId, CommunicationInterface, Core, CoreInterface, CoreList,
    CoreRegisterAddress,
};
pub use crate::error::Error;
pub use crate::memory::{Memory, MemoryInterface, MemoryList};
pub use crate::probe::{DebugProbe, DebugProbeError, DebugProbeInfo, Probe, WireProtocol};
pub use crate::session::Session;
