//! Foobarino
//!
//! This is some documentation

#![allow(clippy::useless_let_if_seq)]
#![allow(clippy::trivially_copy_pass_by_ref)]
#![allow(clippy::implicit_hasher)]

#[macro_use]
pub extern crate derivative;
#[macro_use]
extern crate rental;
#[macro_use]
extern crate maplit;
#[macro_use]
extern crate serde_derive;

pub mod config;
pub mod cores;
pub mod coresight;
pub mod debug;
mod error;
pub mod flash;
pub mod probe;
mod session;
pub mod target;

pub use error::Error;
pub use session::Session;
