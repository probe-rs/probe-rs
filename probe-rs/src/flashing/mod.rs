//! Flash programming operations.
//!
//! This modules provides a means to do flash unlocking, erasing and programming.
//!
//! It provides a convenient highlevel interface that can flash an ELF, IHEX or BIN file
//! as well as a lower level block based interface.

mod builder;
mod download;
mod error;
mod flasher;
mod loader;
mod progress;
mod visualizer;

use builder::*;
pub use download::*;
pub use error::*;
pub use flasher::*;
use loader::*;
pub use progress::*;
pub use visualizer::*;
