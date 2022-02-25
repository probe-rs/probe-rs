//! Flash programming operations.
//!
//! This modules provides a means to do flash unlocking, erasing and programming.
//!
//! It provides a convenient highlevel interface that can flash an ELF, IHEX or BIN file
//! as well as a lower level block based interface.
//!
//!
//! ## Examples
//!
//! ### Flashing a binary
//!
//! The easiest way to flash a binary is using the [`download_file`] function,
//! and looks like this:
//!
//! ```no_run
//! use probe_rs::{Session, flashing, Permissions};
//!
//! let mut session = Session::auto_attach("nrf51822", Permissions::default())?;
//!
//! flashing::download_file(&mut session, "binary.hex", flashing::Format::Hex)?;
//!
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! ### Adding data manually
//!
//! ```no_run
//! use probe_rs::{Session, flashing::{FlashLoader, DownloadOptions}, Permissions};
//!
//!
//! let mut session = Session::auto_attach("nrf51822", Permissions::default())?;
//!
//! let mut loader = session.target().flash_loader();
//!
//! loader.add_data(0x1000_0000, &[0x1, 0x2, 0x3])?;
//!
//! // Finally, the data can be programmed:
//! loader.commit(&mut session, DownloadOptions::default())?;
//!
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//!

mod builder;
mod download;
mod erase;
mod error;
mod flash_algorithm;
mod flasher;
mod loader;
mod progress;
mod visualizer;

use builder::*;
use flasher::*;

pub use download::*;
pub use erase::*;
pub use error::*;
pub use flash_algorithm::*;
pub use loader::*;
pub use progress::*;
pub use visualizer::*;
