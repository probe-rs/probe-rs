//! Compare backend that uses the CMSIS `Verify` function of the flash algorithm.

use super::{CompareContext, CompareCost, CompareOutcome, FlashCompare, SectorCheck};
use crate::config::NvmRegion;
use crate::flashing::{ActiveFlasher, FlashError, Verify};

/// Sends the expected content of each sector to the CMSIS `Verify` function.
#[derive(Debug)]
pub(crate) struct CmsisVerifyCompare;

impl CmsisVerifyCompare {
    fn compare_check(
        active: &mut ActiveFlasher<'_, '_, Verify>,
        check: &SectorCheck<'_>,
    ) -> Result<CompareOutcome, FlashError> {
        for (address, data) in check.contiguous_ranges() {
            if !active.cmsis_verify(address, &data)? {
                return Ok(CompareOutcome::Mismatch);
            }
        }

        Ok(CompareOutcome::Match)
    }
}

impl FlashCompare for CmsisVerifyCompare {
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
            let outcome = active
                .compare_after_blank_check(check, |active| Self::compare_check(active, check))?;
            check.outcome = Some(outcome);
        }

        Ok(())
    }
}
