//! Types representing the current state of the language server.

use std::fmt::{self, Debug, Formatter};
use std::sync::atomic::{AtomicU8, Ordering};

/// A list of possible states the language server can be in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum State {
    /// Server has not received an `initialize` request.
    Uninitialized = 0,
    /// Server received an `initialize` request, but has not yet responded.
    Initializing = 1,
    /// Server received and responded success to an `initialize` request.
    Initialized = 2,
    /// Server received a `shutdown` request.
    ShutDown = 3,
    /// Server received an `exit` notification.
    Exited = 4,
}

/// Atomic value which represents the current state of the server.
pub struct ServerState(AtomicU8);

impl ServerState {
    pub const fn new() -> Self {
        ServerState(AtomicU8::new(State::Uninitialized as u8))
    }

    pub fn set(&self, state: State) {
        self.0.store(state as u8, Ordering::SeqCst);
    }

    pub fn get(&self) -> State {
        match self.0.load(Ordering::SeqCst) {
            0 => State::Uninitialized,
            1 => State::Initializing,
            2 => State::Initialized,
            3 => State::ShutDown,
            4 => State::Exited,
            _ => unreachable!(),
        }
    }
}

impl Debug for ServerState {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        self.get().fmt(f)
    }
}
