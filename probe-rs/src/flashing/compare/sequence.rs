//! Compare backend that uses the [`FlashVerify`] sequence of the target.

use std::sync::Arc;

use super::{CompareContext, CompareCost, CompareOutcome, FlashCompare, SectorCheck};
use crate::config::NvmRegion;
use crate::flashing::{FlashError, FlashVerify, VerifyOutcome};

/// Uses a [`FlashVerify`] implementation as a compare backend.
#[derive(Debug)]
pub(crate) struct FlashVerifyCompare(pub(crate) Arc<dyn FlashVerify>);

impl FlashCompare for FlashVerifyCompare {
    fn cost(&self) -> CompareCost {
        CompareCost::Cheap
    }

    fn compare(
        &self,
        context: &mut CompareContext<'_>,
        region: &NvmRegion,
        checks: &mut [SectorCheck<'_>],
    ) -> Result<(), FlashError> {
        let mut core = context.core()?;
        for check in checks {
            let content = check.sector_content();
            let ranges = match &content {
                Some(content) => vec![(check.sector.start, content.as_slice())],
                None => check.expected.clone(),
            };

            let mut outcome = Some(CompareOutcome::Match);
            for (address, data) in ranges {
                let result = self
                    .0
                    .verify_range(&mut core, region, address, data)
                    .map_err(FlashError::Core)?;
                match result {
                    VerifyOutcome::Match => {}
                    VerifyOutcome::Mismatch => {
                        outcome = Some(CompareOutcome::Mismatch);
                        break;
                    }
                    VerifyOutcome::Unsupported => {
                        outcome = None;
                        break;
                    }
                }
            }
            check.outcome = outcome;
        }

        Ok(())
    }
}
