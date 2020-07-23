//! # Debugging toolset for embedded devices
//!
//!  
//! # Prerequisites
//!
//! - Udev rules
//! - libusb
//!
//! # Examples
//!
//!
//! ## Halting the attached chip
//! ```no_run
//! # use probe_rs::Error;
//! use probe_rs::Probe;
//!
//! // Get a list of all available debug probes.
//! let probes = Probe::list_all();
//!
//! // Use the first probe found.
//! let mut probe = probes[0].open()?;
//!
//! // Attach to a chip.
//! let mut session = probe.attach("nrf52")?;
//!
//! // Select a core.
//! let mut core = session.core(0)?;
//!
//! // Halt the attached core.
//! core.halt(std::time::Duration::from_millis(10))?;
//! # Ok::<(), Error>(())
//! ```
//!
//! ## Reading from RAM
//!
//! ```no_run
//! # use probe_rs::Error;
//! use probe_rs::Session;
//! use probe_rs::MemoryInterface;
//!
//! let mut session = Session::auto_attach("nrf52")?;
//! let mut core = session.core(0)?;
//!
//! // Read a block of 50 32 bit words.
//! let mut buff = [0u32;50];
//! core.read_32(0x2000_0000, &mut buff)?;
//!
//! // Read a single 32 bit word.
//! let word = core.read_word_32(0x2000_0000)?;
//!
//! // Writing is just as simple.
//! let buff = [0u32;50];
//! core.write_32(0x2000_0000, &buff)?;
//!
//! // of course we can also write 8bit words.
//! let buff = [0u8;50];
//! core.write_8(0x2000_0000, &buff)?;
//!
//! # Ok::<(), Error>(())
//! ```
//!
//! probe-rs is built around 5 main interfaces: the [Probe](./struct.Probe.html),
//! [Target](./struct.Target.html), [Session](./struct.Session.html), [Memory](./struct.Memory.html) and [Core](./struct.Core.html) strucs.

#![allow(clippy::useless_let_if_seq)]
#![allow(clippy::trivially_copy_pass_by_ref)]
#![allow(clippy::implicit_hasher)]
#![allow(clippy::verbose_bit_mask)]

#[macro_use]
extern crate serde;

pub mod architecture;
pub mod config;
mod core;
pub mod debug;
mod error;
pub mod flashing;
mod memory;
mod probe;
mod session;

pub use crate::config::Target;
pub use crate::core::CoreType;
pub use crate::core::{
    Architecture, Breakpoint, BreakpointId, CommunicationInterface, Core, CoreInformation,
    CoreInterface, CoreList, CoreRegister, CoreRegisterAddress, CoreStatus, HaltReason,
};
pub use crate::error::Error;
pub use crate::memory::{Memory, MemoryInterface, MemoryList};
pub use crate::probe::{
    AttachMethod, DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, DebugProbeType,
    Probe, WireProtocol,
};
pub use crate::session::Session;
