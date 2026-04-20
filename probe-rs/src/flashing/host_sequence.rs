//! Architecture-agnostic host-side flash sequence trait.
//!
//! [`DebugFlashSequence`] is the interface that vendor implementations provide to perform
//! host-side flash programming.  It is intentionally architecture-agnostic: the methods receive
//! a [`Session`] rather than an architecture-specific debug interface so that implementations
//! can call into ARM, RISC-V, or Xtensa debug infrastructure as needed, or bypass the probe
//! connection entirely and delegate to an external tool (e.g. the SimpleLink WiFi Toolbox for
//! TI CC35xx devices).

use std::fmt::Debug;

use crate::flashing::FlashLayout;
use probe_rs_target::NvmRegion;

use crate::Error;
use crate::session::Session;

/// Host-side flash programming interface.
///
/// Vendors implement this trait to support `flash_loader_type: host_side` in target YAML files.
/// The implementation is returned by the debug sequence via
/// `ArmDebugSequence::debug_flash_sequence` (or the equivalent RISC-V/Xtensa method) and is
/// called by [`HostSideFlasher`](super::HostSideFlasher) during the flash procedure.
///
/// ## Lifecycle
///
/// ```text
/// prepare_flash()    ← enter required mode (e.g. SACI for CC27xx, release probe for CC35xx)
///   erase_all() / erase_sector()
///   program() ...    ← per-page, OR
///   program_image()  ← whole-image (overrides per-page loop when Some is returned)
///   verify() ...
/// finish_flash()     ← exit mode / re-acquire probe; called after both full flash and verify
///
/// [standalone verify pass — same bookends:]
/// prepare_flash()
///   verify() ...
/// finish_flash()
/// ```
pub trait DebugFlashSequence: Send + Sync + Debug {
    /// Called once before any erase/program/verify operations begin.
    ///
    /// Use this to enter a required programming mode or, for external-toolbox devices,
    /// to release the probe connection so that the toolbox can acquire exclusive hardware
    /// access.  [`finish_flash`](Self::finish_flash) is the symmetric counterpart.
    ///
    /// The default implementation does nothing.
    fn prepare_flash(&self, _session: &mut Session) -> Result<(), Error> {
        Ok(())
    }

    /// Erase all flash memory.
    ///
    /// # Errors
    /// May fail due to communication issues or if the device is locked.
    fn erase_all(&self, session: &mut Session) -> Result<(), Error>;

    /// Erase a sector at the given address.
    ///
    /// Only called when [`supports_sector_erase`](Self::supports_sector_erase) returns `true`.
    /// The default returns [`Error::NotImplemented`]; devices that support per-sector erase
    /// should override both this method and `supports_sector_erase`.
    fn erase_sector(&self, _session: &mut Session, _address: u64) -> Result<(), Error> {
        Err(Error::NotImplemented("sector erase"))
    }

    /// Program data to flash at the given address.
    ///
    /// Called once per page by [`HostSideFlasher`](super::HostSideFlasher) unless
    /// [`program_image`](Self::program_image) returns `Some`.
    fn program(&self, session: &mut Session, address: u64, data: &[u8]) -> Result<(), Error>;

    /// Program the entire flash image in one operation.
    ///
    /// Devices that require a whole-image operation (e.g. toolbox-based devices like CC35xx)
    /// override this method.  When it returns `Some`, the per-page `program()` loop is skipped.
    ///
    /// The default returns `None`, falling back to the per-page `program()` calls.
    fn program_image(
        &self,
        _session: &mut Session,
        _regions: &[(&NvmRegion, &FlashLayout)],
    ) -> Option<Result<(), Error>> {
        None
    }

    /// Verify that the data at the given address matches `data`.
    ///
    /// Returns `Ok(true)` if verification passed, `Ok(false)` on mismatch.
    fn verify(&self, session: &mut Session, address: u64, data: &[u8]) -> Result<bool, Error>;

    /// Returns whether this device supports erasing the entire chip in one operation.
    ///
    /// When `true` (the default), the flash loader may use [`erase_all`](Self::erase_all)
    /// as its primary erase strategy.  Devices that manage erase internally (e.g. a
    /// toolbox-based device that performs erase as part of its own programming sequence)
    /// should return `false` so the loader does not issue a redundant separate erase.
    ///
    /// Defaults to `true`.
    fn supports_chip_erase(&self) -> bool {
        true
    }

    /// Returns whether this device supports per-sector erase.
    ///
    /// When `false`, [`HostSideFlasher`](super::HostSideFlasher) performs a single chip erase
    /// via [`erase_all`](Self::erase_all) before programming instead of calling
    /// [`erase_sector`](Self::erase_sector) for each sector.
    ///
    /// Defaults to `true`.
    fn supports_sector_erase(&self) -> bool {
        true
    }

    /// Called after all flash operations (erase, program, verify) complete.
    ///
    /// Use this to exit any special programming mode and leave the device in a clean,
    /// debuggable state.  For SACI-based devices this exits SACI so the AHB-AP is accessible
    /// again.  For external-toolbox devices this re-acquires the probe connection that was
    /// released in [`prepare_flash`](Self::prepare_flash).
    ///
    /// Called at the end of both the full flash path and the standalone verify path.
    ///
    /// The default implementation does nothing.
    fn finish_flash(&self, _session: &mut Session) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A minimal no-op implementation used to verify default method behaviour.
    #[derive(Debug)]
    struct NoOpSequence;

    impl DebugFlashSequence for NoOpSequence {
        fn erase_all(&self, _session: &mut Session) -> Result<(), Error> {
            Ok(())
        }
        fn program(
            &self,
            _session: &mut Session,
            _address: u64,
            _data: &[u8],
        ) -> Result<(), Error> {
            Ok(())
        }
        fn verify(
            &self,
            _session: &mut Session,
            _address: u64,
            _data: &[u8],
        ) -> Result<bool, Error> {
            Ok(true)
        }
    }

    /// Verifies that optional methods have sensible defaults without needing a real device.
    #[test]
    fn default_methods_do_not_require_override() {
        let seq = NoOpSequence;

        // supports_sector_erase defaults to true
        assert!(seq.supports_sector_erase());

        // program_image defaults to None — the method exists with a default implementation.
        // We verify this by confirming NoOpSequence compiles without overriding it.

        // prepare_flash, finish_flash, erase_sector all have defaults
        // (compilation proves the defaults exist and have the expected signatures)
    }

    /// Verifies that the trait is object-safe (can be used as dyn DebugFlashSequence).
    #[test]
    fn trait_is_object_safe() {
        let seq: Arc<dyn DebugFlashSequence> = Arc::new(NoOpSequence);
        assert!(seq.supports_sector_erase());
    }

    /// Verifies that a sequence satisfies the Send + Sync bounds.
    #[test]
    fn sequence_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NoOpSequence>();
    }

    /// Verifies the Debug bound is satisfied.
    #[test]
    fn sequence_implements_debug() {
        let seq = NoOpSequence;
        let s = format!("{seq:?}");
        assert!(!s.is_empty());
    }
}
