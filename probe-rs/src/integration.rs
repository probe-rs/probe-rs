//! Helper functions for integration tests in your application using probe-rs.
//!
//! The fake probe needs the `test` feature.

#[cfg(any(test, feature = "test"))]
pub use crate::probe::fake_probe::{FakeProbe, FakeProbeFactory, Operation};
pub use crate::probe::list::ProbeLister;
