//! Sequences for Atmel targets.

use crate::architecture::arm::ArmProbeInterface;
use crate::error::Error;
use crate::Target;

pub(crate) fn detect_target(
    probe_interface: &mut Box<dyn ArmProbeInterface>,
) -> Result<Target, Error> {
    todo!();
}
