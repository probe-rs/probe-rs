//! # Debugging toolset for embedded devices
//!
//!
//! # Prerequisites
//!
//! - Udev rules
//!
//! # Examples
//!
//!
//! ## Halting the attached chip
//! ```no_run
//! # use probe_rs::Error;
//! use probe_rs::probe::{list::Lister, Probe};
//! use probe_rs::Permissions;
//!
//! // Get a list of all available debug probes.
//! let lister = Lister::new();
//!
//! let probes = lister.list_all();
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
//! use probe_rs::{Session, Permissions, MemoryInterface};
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
//!
//! [Probe]: probe::Probe
#![warn(missing_docs)]
#![recursion_limit = "256"]

#[macro_use]
extern crate serde;

/// All the interface bits for the different architectures.
pub mod architecture;
pub mod config;

mod core;
pub mod debug;
mod error;
pub mod flashing;
#[cfg(feature = "gdb-server")]
pub mod gdb_server;
pub mod integration;
mod memory;
pub mod probe;
#[cfg(feature = "rtt")]
pub mod rtt;
mod semihosting;
mod session;
#[cfg(test)]
mod test;

pub use crate::config::{CoreType, InstructionSet, Target};
pub use crate::core::{
    dump::{CoreDump, CoreDumpError},
    exception_handler_for_core, Architecture, BreakpointCause, Core, CoreInformation,
    CoreInterface, CoreRegister, CoreRegisters, CoreState, CoreStatus, HaltReason,
    MemoryMappedRegister, RegisterId, RegisterRole, RegisterValue, SpecificCoreState,
    VectorCatchCondition,
};
pub use crate::error::Error;
pub use crate::memory::MemoryInterface;
pub use crate::semihosting::{
    ExitErrorDetails, GetCommandLineRequest, SemihostingCommand, UnknownCommandDetails,
};
pub use crate::session::{Permissions, Session};
