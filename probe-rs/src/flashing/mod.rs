#![warn(missing_docs)]

//! Flash programming operations.
//!
//! This modules provides a means to do flash unlocking, erasing and programming.
//!
//! It provides a convenient highlevel interface that can flash an ELF, IHEX or BIN file
//! as well as a lower level block based interface.

mod builder;
mod download;
mod error;
mod flash_algorithm;
mod flasher;
mod loader;
mod progress;
mod visualizer;

use builder::*;
pub use download::*;
pub use error::*;
pub use flash_algorithm::*;
pub use flasher::*;
pub use progress::*;
pub use visualizer::*;

pub use loader::FlashLoader;
