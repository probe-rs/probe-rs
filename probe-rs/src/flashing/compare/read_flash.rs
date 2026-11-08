//! Compare backend that uses the `ReadFlash` function of the flash algorithm.

use super::blank_check::read_after_blank_check;
use super::{CompareContext, CompareCost, FlashCompare, SectorCheck};
use crate::config::NvmRegion;
use crate::flashing::FlashError;

/// Reads the flash contents back through the `ReadFlash` function.
#[derive(Debug)]
pub(crate) struct ReadFlashCompare;

impl FlashCompare for ReadFlashCompare {
    fn name(&self) -> &str {
        "ReadFlash"
    }

    fn cost(&self) -> CompareCost {
        CompareCost::Costly
    }

    fn compare(
        &self,
        context: &mut CompareContext<'_>,
        _region: &NvmRegion,
        checks: &mut [SectorCheck<'_>],
    ) -> Result<(), FlashError> {
        read_after_blank_check(context, checks)
    }
}
