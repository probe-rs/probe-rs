//! Compare flash contents with the image.
//!
//! A [`FlashCompare`] backend checks whole erase sectors. The flasher runs its backends in a
//! fixed order, and gives each backend only the sectors that the backends before it could not
//! check.

mod signature;

pub(crate) use signature::AlgorithmSignatures;

use std::fmt::Debug;
use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use crate::config::NvmRegion;
use crate::flashing::{
    ActiveFlasher, FlashError, FlashLayout, FlashProgress, ProgressOperation, Verify,
};
use crate::memory::MemoryInterface;
use crate::{Error, Session};

/// One erase sector, and the content that it must hold.
#[derive(Debug, Clone)]
pub struct SectorCheck<'a> {
    /// The address range of the erase sector.
    pub sector: Range<u64>,
    /// The image data in the sector, as `(address, data)` pairs in ascending address order.
    pub expected: Vec<(u64, &'a [u8])>,
    /// The content of the bytes that `expected` does not cover.
    pub outside: Outside,
}

impl SectorCheck<'_> {
    /// Returns the full content of the sector, if the check defines it.
    pub fn sector_content(&self) -> Option<Vec<u8>> {
        let Outside::Erased(erased) = self.outside else {
            return None;
        };

        let mut content = vec![erased; (self.sector.end - self.sector.start) as usize];
        for &(address, data) in &self.expected {
            let offset = (address - self.sector.start) as usize;
            content[offset..][..data.len()].copy_from_slice(data);
        }

        Some(content)
    }
}

/// The content of the bytes of a sector that the image does not cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outside {
    /// Any value is correct.
    Any,
    /// Each byte holds this erased value.
    Erased(u8),
}

/// The result of a compare for one sector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOutcome {
    /// The sector holds the expected content.
    Match,
    /// The sector does not hold the expected content. It needs an erase.
    Mismatch,
    /// Each byte of the sector holds the erased value, and the expected content is not
    /// erased. The sector needs programming, but no erase.
    Empty,
    /// This backend cannot check this sector.
    Unsupported,
}

/// How much data a backend moves over the debug link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareCost {
    /// The backend sends addresses and receives signatures.
    Cheap,
    /// The backend moves the sector data over the debug link.
    Costly,
}

/// A method to compare flash sectors with their expected content.
///
/// Rules for an implementation:
///
/// - Never give [`CompareOutcome::Match`] or [`CompareOutcome::Empty`] when it is not true.
/// - [`CompareOutcome::Mismatch`] in place of [`CompareOutcome::Empty`] is correct. It only
///   costs an erase.
/// - Give [`CompareOutcome::Unsupported`] for a sector that this method cannot check. Do not
///   use it to hide an error. Return the error.
/// - Do not read erased flash cells on a target where such a read faults.
pub trait FlashCompare: Send + Sync + Debug {
    /// Returns how much data this method moves over the debug link.
    fn cost(&self) -> CompareCost;

    /// Sets `out[i]` to the result for `checks[i]`.
    ///
    /// `out` holds [`CompareOutcome::Unsupported`] on entry. All checks are in `region`.
    fn compare(
        &self,
        session: &mut Session,
        core_index: usize,
        region: &NvmRegion,
        checks: &[SectorCheck<'_>],
        out: &mut [CompareOutcome],
    ) -> Result<(), Error>;
}

/// Builds one check for each sector of `layout`.
///
/// If `ignore_fills` is `true`, the fill bytes are not part of the expected data.
pub(crate) fn sector_checks(
    layout: &FlashLayout,
    outside: Outside,
    ignore_fills: bool,
) -> Vec<SectorCheck<'_>> {
    layout
        .sectors()
        .iter()
        .map(|sector| {
            let range = sector.address()..sector.address() + sector.size();

            let mut expected: Vec<(u64, &[u8])> = Vec::new();
            for (idx, page) in layout.pages().iter().enumerate() {
                let page_range = page.address()..page.address() + page.size() as u64;
                if page_range.end <= range.start || range.end <= page_range.start {
                    continue;
                }

                let pieces = if ignore_fills {
                    layout.page_data_without_fills(idx)
                } else {
                    vec![(page.address(), page.data())]
                };

                expected.extend(pieces.into_iter().filter_map(|(address, data)| {
                    let start = address.max(range.start);
                    let end = (address + data.len() as u64).min(range.end);
                    (start < end).then(|| {
                        let offset = (start - address) as usize;
                        (start, &data[offset..offset + (end - start) as usize])
                    })
                }));
            }

            SectorCheck {
                sector: range,
                expected,
                outside,
            }
        })
        .collect()
}

/// Tracks the result of each check while the backends run.
pub(crate) struct CompareState {
    outcomes: Vec<CompareOutcome>,
}

impl CompareState {
    pub(crate) fn new(count: usize) -> Self {
        Self {
            outcomes: vec![CompareOutcome::Unsupported; count],
        }
    }

    /// Returns the indices of the checks that have no result yet.
    pub(crate) fn pending(&self) -> Vec<usize> {
        self.outcomes
            .iter()
            .enumerate()
            .filter(|(_, outcome)| **outcome == CompareOutcome::Unsupported)
            .map(|(idx, _)| idx)
            .collect()
    }

    /// Stores the results of a backend for the checks at `indices`.
    pub(crate) fn apply(&mut self, indices: &[usize], results: &[CompareOutcome]) {
        for (&idx, &result) in indices.iter().zip(results) {
            self.outcomes[idx] = result;
        }
    }

    /// Returns the results. A check that no backend could do counts as a mismatch.
    pub(crate) fn finish(self) -> Vec<CompareOutcome> {
        self.outcomes
            .into_iter()
            .map(|outcome| match outcome {
                CompareOutcome::Unsupported => CompareOutcome::Mismatch,
                other => other,
            })
            .collect()
    }
}

/// Compares flash contents that `read` reads back with the check.
pub(crate) fn compare_read_back<E>(
    check: &SectorCheck<'_>,
    mut read: impl FnMut(u64, &mut [u8]) -> Result<(), E>,
) -> Result<CompareOutcome, E> {
    if let Some(content) = check.sector_content() {
        let Outside::Erased(erased) = check.outside else {
            unreachable!()
        };

        let mut read_back = vec![0; content.len()];
        read(check.sector.start, &mut read_back)?;

        return Ok(if read_back == content {
            CompareOutcome::Match
        } else if read_back.iter().all(|&byte| byte == erased) {
            CompareOutcome::Empty
        } else {
            CompareOutcome::Mismatch
        });
    }

    for &(address, data) in &check.expected {
        let mut read_back = vec![0; data.len()];
        read(address, &mut read_back)?;
        if read_back != data {
            return Ok(CompareOutcome::Mismatch);
        }
    }

    Ok(CompareOutcome::Match)
}

/// Reads the flash contents back through the memory interface of the core.
#[derive(Debug)]
pub(crate) struct ReadBackCore;

impl FlashCompare for ReadBackCore {
    fn cost(&self) -> CompareCost {
        CompareCost::Costly
    }

    fn compare(
        &self,
        session: &mut Session,
        core_index: usize,
        _region: &NvmRegion,
        checks: &[SectorCheck<'_>],
        out: &mut [CompareOutcome],
    ) -> Result<(), Error> {
        let mut core = session.core(core_index)?;
        for (check, out) in checks.iter().zip(out.iter_mut()) {
            *out = compare_read_back(check, |address, data| core.read(address, data))?;
        }

        Ok(())
    }
}

/// A compare method that needs the flash algorithm in target RAM.
pub(crate) trait AlgorithmCompare: Debug {
    fn cost(&self) -> CompareCost;

    /// Sets `out[i]` to the result for `checks[i]`.
    fn compare(
        &self,
        active: &mut ActiveFlasher<'_, '_, Verify>,
        region: &NvmRegion,
        checks: &[SectorCheck<'_>],
        out: &mut [CompareOutcome],
    ) -> Result<(), FlashError>;
}

/// Reads the flash contents back through the `ReadFlash` function of the flash algorithm.
#[derive(Debug)]
pub(crate) struct ReadBackAlgorithm;

impl AlgorithmCompare for ReadBackAlgorithm {
    fn cost(&self) -> CompareCost {
        CompareCost::Costly
    }

    fn compare(
        &self,
        active: &mut ActiveFlasher<'_, '_, Verify>,
        _region: &NvmRegion,
        checks: &[SectorCheck<'_>],
        out: &mut [CompareOutcome],
    ) -> Result<(), FlashError> {
        for (check, out) in checks.iter().zip(out.iter_mut()) {
            *out = compare_read_back(check, |address, data| active.read_flash(address, data))?;
        }

        Ok(())
    }
}

/// Sends the expected data to the CMSIS `Verify` function of the flash algorithm.
#[derive(Debug)]
pub(crate) struct CmsisVerify;

impl AlgorithmCompare for CmsisVerify {
    fn cost(&self) -> CompareCost {
        CompareCost::Costly
    }

    fn compare(
        &self,
        active: &mut ActiveFlasher<'_, '_, Verify>,
        _region: &NvmRegion,
        checks: &[SectorCheck<'_>],
        out: &mut [CompareOutcome],
    ) -> Result<(), FlashError> {
        for (check, out) in checks.iter().zip(out.iter_mut()) {
            let content = check.sector_content();
            let ranges = match &content {
                Some(content) => vec![(check.sector.start, content.as_slice())],
                None => check.expected.clone(),
            };

            *out = CompareOutcome::Match;
            for (address, data) in ranges {
                if !active.cmsis_verify(address, data)? {
                    *out = CompareOutcome::Mismatch;
                    break;
                }
            }
        }

        Ok(())
    }
}

/// A compare backend in the chain of a flasher.
pub(crate) enum CompareBackend {
    /// A backend that needs a session only.
    Session(Arc<dyn FlashCompare>),
    /// A backend that needs the flash algorithm in target RAM.
    Algorithm(Box<dyn AlgorithmCompare>),
}

/// One check, with the index of its region and the progress size of its sector.
pub(crate) type IndexedCheck<'a> = (usize, SectorCheck<'a>, u64);

/// Builds the checks for the sectors of each layout.
///
/// The progress size of a sector is the size of its pages.
pub(crate) fn build_checks<'a>(
    layouts: &[&'a FlashLayout],
    outside: Outside,
    ignore_fills: bool,
) -> Vec<IndexedCheck<'a>> {
    let mut checks = Vec::new();
    for (region_index, layout) in layouts.iter().enumerate() {
        for check in sector_checks(layout, outside, ignore_fills) {
            let progress_size = layout
                .pages()
                .iter()
                .filter_map(|page| {
                    let start = page.address().max(check.sector.start);
                    let end = (page.address() + page.size() as u64).min(check.sector.end);
                    (start < end).then_some(end - start)
                })
                .sum::<u64>();
            checks.push((region_index, check, progress_size));
        }
    }
    checks
}

/// Groups the results of `checks` by region.
pub(crate) fn split_by_region(
    checks: &[IndexedCheck<'_>],
    outcomes: Vec<CompareOutcome>,
    region_count: usize,
) -> Vec<Vec<CompareOutcome>> {
    let mut per_region = vec![Vec::new(); region_count];
    for ((region_index, _, _), outcome) in checks.iter().zip(outcomes) {
        per_region[*region_index].push(outcome);
    }
    per_region
}

/// Runs one backend on the checks that have no result yet.
///
/// `backend` names the backend in the log. `checks` holds the region index, the check, and the
/// progress size of each sector. A cheap backend gets all sectors of a region in one call. A
/// costly backend gets one sector in each call, so that the progress moves while it runs.
#[expect(clippy::too_many_arguments)]
pub(crate) fn run_backend(
    backend: &dyn Debug,
    regions: &[NvmRegion],
    checks: &[IndexedCheck<'_>],
    state: &mut CompareState,
    cost: CompareCost,
    progress: &mut FlashProgress<'_>,
    operation: ProgressOperation,
    mut run: impl FnMut(&NvmRegion, &[SectorCheck<'_>], &mut [CompareOutcome]) -> Result<(), FlashError>,
) -> Result<(), FlashError> {
    let pending = state.pending();

    for (region_index, region) in regions.iter().enumerate() {
        let indices = pending
            .iter()
            .copied()
            .filter(|&idx| checks[idx].0 == region_index)
            .collect::<Vec<_>>();
        let batch_size = match cost {
            CompareCost::Cheap => indices.len().max(1),
            CompareCost::Costly => 1,
        };

        for batch_indices in indices.chunks(batch_size) {
            let started = Instant::now();
            let batch = batch_indices
                .iter()
                .map(|&idx| checks[idx].1.clone())
                .collect::<Vec<_>>();
            let mut out = vec![CompareOutcome::Unsupported; batch.len()];

            run(region, &batch, &mut out)?;
            state.apply(batch_indices, &out);

            for (check, outcome) in batch.iter().zip(&out) {
                tracing::debug!("{backend:?}: sector {:#010x?}: {outcome:?}", check.sector);
            }

            let done = batch_indices
                .iter()
                .zip(&out)
                .filter(|(_, outcome)| **outcome != CompareOutcome::Unsupported)
                .map(|(&idx, _)| checks[idx].2)
                .sum::<u64>();
            if done > 0 {
                progress.progressed(operation, done, started.elapsed());
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use probe_rs_target::{FlashProperties, SectorDescription};

    use super::*;
    use crate::flashing::FlashBuilder;

    const ERASED: u8 = 0xFF;

    /// Four 1 KiB pages in each 4 KiB sector.
    fn layout(data: &[(u64, &[u8])], include_empty_pages: bool) -> FlashLayout {
        layout_with_geometry(data, include_empty_pages, 0x400, 0x1000)
    }

    fn layout_with_geometry(
        data: &[(u64, &[u8])],
        include_empty_pages: bool,
        page_size: u32,
        sector_size: u64,
    ) -> FlashLayout {
        let range = 0..u64::from(page_size.max(0x4000));
        let props = FlashProperties {
            address_range: range.clone(),
            page_size,
            erased_byte_value: ERASED,
            program_page_timeout: 100,
            erase_sector_timeout: 100,
            sectors: vec![SectorDescription {
                size: sector_size,
                address: 0,
            }],
        };
        let region = NvmRegion {
            name: None,
            access: None,
            range,
            cores: vec!["main".into()],
            memory_ports: vec![],
            is_alias: false,
        };

        let mut builder = FlashBuilder::new();
        for &(address, bytes) in data {
            builder.add_data(address, bytes).unwrap();
        }
        builder
            .build_sectors_and_pages_from_properties(&region, &props, include_empty_pages)
            .unwrap()
    }

    #[test]
    fn checks_hold_the_image_data_of_each_sector() {
        let layout = layout(&[(0x10, &[1, 2]), (0x1400, &[3])], false);

        let checks = sector_checks(&layout, Outside::Any, true);

        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].sector, 0x0000..0x1000);
        assert_eq!(checks[0].expected, vec![(0x10, &[1, 2][..])]);
        assert_eq!(checks[1].sector, 0x1000..0x2000);
        assert_eq!(checks[1].expected, vec![(0x1400, &[3][..])]);
    }

    #[test]
    fn checks_split_pages_at_sector_boundaries() {
        let data = [0x5a; 128];
        let layout = layout_with_geometry(&[(0x6000, &data)], false, 0x8000, 0x2000);

        let checks = sector_checks(&layout, Outside::Any, true);

        assert_eq!(checks.len(), 4);
        assert!(checks[0].expected.is_empty());
        assert_eq!(checks[3].expected, vec![(0x6000, data.as_slice())]);

        let progress_sizes = build_checks(&[&layout], Outside::Any, true)
            .into_iter()
            .map(|(_, _, size)| size)
            .collect::<Vec<_>>();
        assert_eq!(progress_sizes, vec![0x2000; 4]);
    }

    #[test]
    fn sector_content_supports_pages_larger_than_sectors() {
        let data = [0x5a; 128];
        let layout = layout_with_geometry(&[(0x6000, &data)], false, 0x8000, 0x2000);

        for check in sector_checks(&layout, Outside::Erased(ERASED), true) {
            assert_eq!(check.sector_content().unwrap().len(), 0x2000);
        }
    }

    #[test]
    fn checks_hold_full_pages_when_fills_count() {
        let layout = layout(&[(0x10, &[1, 2])], false);

        let checks = sector_checks(&layout, Outside::Any, false);

        assert_eq!(checks[0].expected.len(), 1);
        assert_eq!(checks[0].expected[0].0, 0);
        assert_eq!(checks[0].expected[0].1.len(), 0x400);
    }

    /// Reads from a simulated flash that starts at address 0.
    fn read_from(flash: &[u8]) -> impl FnMut(u64, &mut [u8]) -> Result<(), ()> + '_ {
        |address, data| {
            data.copy_from_slice(&flash[address as usize..][..data.len()]);
            Ok(())
        }
    }

    #[test]
    fn read_back_with_erased_outside() {
        let layout = layout(&[(0x10, &[1, 2])], false);
        let checks = sector_checks(&layout, Outside::Erased(ERASED), true);

        let mut flash = vec![ERASED; 0x1000];
        assert_eq!(
            compare_read_back(&checks[0], read_from(&flash)),
            Ok(CompareOutcome::Empty)
        );

        flash[0x10..0x12].copy_from_slice(&[1, 2]);
        assert_eq!(
            compare_read_back(&checks[0], read_from(&flash)),
            Ok(CompareOutcome::Match)
        );

        // An old byte outside the image needs an erase.
        flash[0x800] = 0;
        assert_eq!(
            compare_read_back(&checks[0], read_from(&flash)),
            Ok(CompareOutcome::Mismatch)
        );
    }

    #[test]
    fn read_back_with_any_outside_ignores_other_bytes() {
        let layout = layout(&[(0x10, &[1, 2])], false);
        let checks = sector_checks(&layout, Outside::Any, true);

        let mut flash = vec![0; 0x1000];
        flash[0x10..0x12].copy_from_slice(&[1, 2]);
        assert_eq!(
            compare_read_back(&checks[0], read_from(&flash)),
            Ok(CompareOutcome::Match)
        );

        flash[0x11] = 0;
        assert_eq!(
            compare_read_back(&checks[0], read_from(&flash)),
            Ok(CompareOutcome::Mismatch)
        );
    }

    #[test]
    fn later_backends_see_only_pending_checks() {
        let mut state = CompareState::new(4);
        state.apply(
            &[0, 1, 2, 3],
            &[
                CompareOutcome::Match,
                CompareOutcome::Unsupported,
                CompareOutcome::Empty,
                CompareOutcome::Unsupported,
            ],
        );
        assert_eq!(state.pending(), vec![1, 3]);

        state.apply(&[1], &[CompareOutcome::Match]);
        assert_eq!(
            state.finish(),
            vec![
                CompareOutcome::Match,
                CompareOutcome::Match,
                CompareOutcome::Empty,
                CompareOutcome::Mismatch,
            ]
        );
    }
}
