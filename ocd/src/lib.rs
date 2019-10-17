#![allow(clippy::useless_let_if_seq)]
#![allow(clippy::trivially_copy_pass_by_ref)]
#![allow(clippy::implicit_hasher)]

#[macro_use]
pub extern crate bitflags;
#[macro_use]
pub extern crate derivative;
#[macro_use]
extern crate rental;
#[macro_use]
extern crate maplit;
#[macro_use]
extern crate serde_derive;

pub mod collection;
pub mod coresight;
pub mod debug;
pub mod memory;
pub mod probe;
pub mod session;
pub mod target;
