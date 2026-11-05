//! Compare backend that uses the `ReadFlash` function of the flash algorithm.

use super::{CompareContext, CompareCost, FlashCompare, SectorCheck};
use crate::config::NvmRegion;
use crate::flashing::FlashError;

/// Reads the flash contents back through the `ReadFlash` function.
#[derive(Debug)]
pub(crate) struct ReadFlashCompare;

impl FlashCompare for ReadFlashCompare {
    fn cost(&self) -> CompareCost {
        CompareCost::Costly
    }

    fn compare(
        &self,
        context: &mut CompareContext<'_>,
        _region: &NvmRegion,
        checks: &mut [SectorCheck<'_>],
    ) -> Result<(), FlashError> {
        let mut active = context.algorithm()?;
        for check in checks {
            let outcome = active.compare_after_blank_check(check, |active| {
                check.compare_read_back(|address, data| active.read_flash(address, data))
            })?;
            check.outcome = Some(outcome);
        }

        Ok(())
    }
}
