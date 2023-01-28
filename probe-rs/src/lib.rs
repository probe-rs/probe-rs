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
//! use probe_rs::{Probe, Permissions};
//!
//! // Get a list of all available debug probes.
//! let probes = Probe::list_all();
//!
//! // Use the first probe found.
//! let mut probe = probes[0].open()?;
//!
//! // Attach to a chip.
//! let mut session = probe.attach("nrf52", Permissions::default())?;
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
//! use probe_rs::{Session, Permissions};
//! use probe_rs::MemoryInterface;
//!
//! let mut session = Session::auto_attach("nrf52", Permissions::default())?;
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
//! probe-rs is built around 4 main interfaces: the [Probe],
//! [Target], [Session]  and [Core] structs.

#![recursion_limit = "256"]

#[macro_use]
extern crate serde;

/// All the interface bits for the different architectures.
pub mod architecture;
pub mod config;

#[warn(missing_docs)]
mod core;
pub mod debug;
mod error;
#[warn(missing_docs)]
pub mod flashing;
#[warn(missing_docs)]
mod memory;
#[warn(missing_docs)]
mod probe;
#[warn(missing_docs)]
mod session;

pub use crate::config::{CoreType, InstructionSet, Target};
pub use crate::core::{
    Architecture, BreakpointCause, BreakpointId, Core, CoreInformation, CoreInterface, CoreState,
    CoreStatus, HaltReason, MemoryMappedRegister, RegisterDescription, RegisterFile, RegisterId,
    RegisterValue, SpecificCoreState,
};
pub use crate::error::Error;
pub use crate::memory::MemoryInterface;
pub use crate::probe::{
    AttachMethod, DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, DebugProbeType,
    Probe, ProbeCreationError, WireProtocol,
};
pub use crate::session::{Permissions, Session};

// TODO: Hide behind feature
pub use crate::probe::fake_probe::FakeProbe;
