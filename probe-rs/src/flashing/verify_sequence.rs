//! Architecture-agnostic fast flash verification.
//!
//! Verifying a flash region normally means reading every byte of it back over the debug
//! link and comparing. That is the slowest part of a download on a fast target, because it
//! moves the whole image across the wire a second time.
//!
//! Some flash controllers can compute a signature over a range of flash themselves, so the
//! host only has to send the range and compare one value. [`FlashVerify`] is the opt-in
//! interface for that: a vendor implements it, returns it from its debug sequence, and the
//! flash loader uses it in preference to reading the contents back.
//!
//! The trait takes a [`Core`] rather than an architecture-specific interface, so an
//! implementation is free to drive memory-mapped flash controller registers on any
//! architecture.

use std::fmt::Debug;

use crate::{Core, Error};

/// Result of a vendor verification attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// The range holds the expected contents.
    Match,
    /// The range does not hold the expected contents.
    Mismatch,
    /// This range cannot be checked this way, so the caller should fall back to reading it
    /// back. Returning this must never be used to paper over an error: a verification that
    /// cannot run is not a verification that passed.
    Unsupported,
}

/// Verify flash contents without transferring them.
///
/// Implementations compare `data` against the flash at `address` using whatever facility the
/// target provides - a flash-controller signature register, a CRC unit, or similar - and are
/// expected to be free of false passes. A signature narrower than the data it covers can
/// collide in principle, so an implementation should use the widest one its hardware offers.
pub trait FlashVerify: Send + Sync + Debug {
    /// Check that flash at `address` holds `data`.
    ///
    /// Return [`VerifyOutcome::Unsupported`] when the range cannot be handled - an
    /// unaligned start, a region the facility does not cover - and the caller will read the
    /// contents back instead.
    fn verify_range(
        &self,
        core: &mut Core<'_>,
        address: u64,
        data: &[u8],
    ) -> Result<VerifyOutcome, Error>;
}
