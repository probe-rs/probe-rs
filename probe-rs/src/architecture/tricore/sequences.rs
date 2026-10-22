//! Debug sequences for TriCore targets.

use std::{fmt::Debug, sync::Arc};

use crate::architecture::tricore::bus::TriCoreBusAccess;
use crate::flashing::DebugFlashSequence;

/// Chip specific sequences of a TriCore target.
///
/// A vendor crate implements this for its chips; the defaults suit any AURIX device.
pub trait TriCoreDebugSequence: Send + Sync + Debug {
    /// Called once after the session connected to the device, before any core is used.
    fn on_connect(&self, _bus: &mut dyn TriCoreBusAccess) -> Result<(), crate::Error> {
        Ok(())
    }

    /// Prepares running an image that was written to RAM on core `core_id`.
    ///
    /// The core is reset and halted before this call.
    fn prepare_running_on_ram(
        &self,
        _session: &mut crate::Session,
        _entry_point: u64,
        _core_id: usize,
    ) -> Result<(), crate::Error> {
        Err(crate::Error::NotImplemented("RAM running on TriCore targets"))
    }

    /// Prepares the target before an image that boots from RAM is written.
    fn prepare_loading_to_ram(&self, _session: &mut crate::Session) -> Result<(), crate::Error> {
        Ok(())
    }

    /// The host-side flash sequence of the chip, if it has one.
    ///
    /// TriCore flash is programmed by the host (with or without a loader in RAM), because the
    /// RAM flash algorithm calling convention of probe-rs is an ARM one.
    fn debug_flash_sequence(&self) -> Option<Arc<dyn DebugFlashSequence>> {
        None
    }

    /// The fast flash verification of the chip, if it has one.
    fn flash_verify_sequence(&self) -> Option<Arc<dyn crate::flashing::FlashVerify>> {
        None
    }
}

/// The sequences used for TriCore chips without a specific sequence.
#[derive(Debug)]
pub struct DefaultTriCoreSequence(pub(crate) ());

impl DefaultTriCoreSequence {
    /// Creates the default TriCore debug sequence.
    pub fn create() -> Arc<dyn TriCoreDebugSequence> {
        Arc::new(Self(()))
    }
}

impl TriCoreDebugSequence for DefaultTriCoreSequence {}
