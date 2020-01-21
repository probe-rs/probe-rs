//! # As short as it gets
//! ```
//! # use probe_rs::probe::{MasterProbe, daplink, DebugProbe};
//! # use probe_rs::probe::daplink::DAPLink;
//! # use probe_rs::config::registry::{Registry, SelectionStrategy, TargetIdentifier};
//! # use probe_rs::{Session, Error};
//!
//! # fn main() -> Result<(), Error> {
//!
//! let registry = Registry::from_builtin_families();
//! let target = registry.get_target(SelectionStrategy::TargetIdentifier(TargetIdentifier {
//!    chip_name: "nrf52".to_owned(),
//!    flash_algorithm_name: None,
//! }))?;
//!
//! let probes = daplink::tools::list_daplink_devices();
//!
//! let specific_probe = DAPLink::new_from_probe_info(probes[0])?;
//!
//! let probe = MasterProbe::from_specific_probe(specific_probe);
//!
//! let session = Session::new(target, probe);
//! # Ok(())
//! # }
//! ```
//! probe-rs is built around 5 main interfaces: the [Probe](./struct.Probe.html),
//! [Target](./struct.Target.html), [Session](./struct.Session.html), [Memory](./struct.Memory.html) and [Core](./struct.Core.html) strucs.

#![allow(clippy::useless_let_if_seq)]
#![allow(clippy::trivially_copy_pass_by_ref)]
#![allow(clippy::implicit_hasher)]
#![allow(clippy::verbose_bit_mask)]

#[macro_use]
pub extern crate derivative;
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
