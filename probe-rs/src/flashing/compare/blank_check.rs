//! Finds empty sectors with the `BlankCheck` function of the flash algorithm.

use super::{CompareContext, CompareOutcome, Outside, SectorCheck};
use crate::flashing::{ActiveFlasher, FlashError, Verify};

/// Sets the result of each check with [`compare_after_blank_check`], and reads the sectors that
/// are not blank through the flash algorithm.
pub(super) fn read_after_blank_check(
    context: &mut CompareContext<'_>,
    checks: &mut [SectorCheck<'_>],
) -> Result<(), FlashError> {
    let mut active = context.algorithm()?;
    for check in checks {
        let outcome = compare_after_blank_check(&mut active, check, |active| {
            check.compare_read_back(|address, data| active.read_flash(address, data))
        })?;
        check.set_outcome(outcome);
    }

    Ok(())
}

/// Runs `BlankCheck` if the check and the algorithm allow it, and `compare` if the sector is
/// not blank.
pub(super) fn compare_after_blank_check<'op, 'p>(
    active: &mut ActiveFlasher<'op, 'p, Verify>,
    check: &SectorCheck<'_>,
    compare: impl FnOnce(&mut ActiveFlasher<'op, 'p, Verify>) -> Result<CompareOutcome, FlashError>,
) -> Result<CompareOutcome, FlashError> {
    let sector = check.sector();

    // On some targets, a read of erased flash faults.
    if let Outside::Erased(erased) = check.outside()
        && active.flash_algorithm().pc_blank_check.is_some()
        && active.is_blank(sector.start, sector.end - sector.start)?
    {
        let expected_erased = check
            .expected()
            .iter()
            .all(|(_, data)| data.iter().all(|&byte| byte == erased));
        let outcome = if expected_erased {
            CompareOutcome::Match
        } else {
            CompareOutcome::Empty
        };
        return Ok(outcome);
    }

    compare(active)
}
