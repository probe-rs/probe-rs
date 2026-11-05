//! Compare backend that reads the flash contents back through the core.

use super::{CompareContext, CompareCost, FlashCompare, Outside, SectorCheck};
use crate::config::NvmRegion;
use crate::flashing::FlashError;
use crate::memory::MemoryInterface;

/// Reads the flash contents back through the memory interface of the core.
#[derive(Debug)]
pub(crate) struct ReadBackCore;

impl FlashCompare for ReadBackCore {
    fn cost(&self) -> CompareCost {
        CompareCost::Costly
    }

    fn compare(
        &self,
        context: &mut CompareContext<'_>,
        _region: &NvmRegion,
        checks: &mut [SectorCheck<'_>],
    ) -> Result<(), FlashError> {
        let uses_blank_check = context
            .flash_algorithm()
            .is_some_and(|algorithm| algorithm.pc_blank_check.is_some())
            && checks
                .iter()
                .any(|check| matches!(check.outside, Outside::Erased(_)));

        // `context.core()` calls `UnInit`, so `BlankCheck` and the read share the algorithm.
        if uses_blank_check {
            let mut active = context.algorithm()?;
            for check in checks {
                let outcome = active.compare_after_blank_check(check, |active| {
                    check.compare_read_back(|address, data| active.read_flash(address, data))
                })?;
                check.outcome = Some(outcome);
            }

            return Ok(());
        }

        let mut core = context.core()?;
        for check in checks {
            let outcome = check.compare_read_back(|address, data| {
                core.read(address, data).map_err(FlashError::Core)
            })?;
            check.outcome = Some(outcome);
        }

        Ok(())
    }
}
