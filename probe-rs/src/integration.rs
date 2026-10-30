//! Helper functions for integration tests in your application using probe-rs.

#[cfg(any(test, feature = "test"))]
pub use crate::probe::fake_probe::FakeProbeFactory;
pub use crate::probe::fake_probe::{FakeProbe, Operation};
pub use crate::probe::list::ProbeLister;
