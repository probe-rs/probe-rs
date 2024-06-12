//! Sequences for FM3

//use crate::architecture::arm::armv7m::{Aircr, Dhcsr, FpCtrl, FpRev1CompX, FpRev2CompX};
use crate::architecture::arm::sequences::ArmDebugSequence;
use std::sync::Arc;

/// An Cypress FM3 MCU.
#[derive(Debug)]
pub struct FM3 {}

impl FM3 {
    /// Create the sequencer for an Infineon XMC4000.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self {})
    }
}

impl ArmDebugSequence for FM3 {}
