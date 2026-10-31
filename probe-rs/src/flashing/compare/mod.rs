//! Compare flash contents with the image.
//!
//! The flasher runs its backends in order. Each backend gets the sectors that no earlier
//! backend could check.

mod cmsis_verify;
mod context;
mod read_back;
mod read_flash;

pub(crate) use cmsis_verify::CmsisVerifyCompare;
pub use context::CompareContext;
pub(crate) use read_back::ReadBackCore;
pub(crate) use read_flash::ReadFlashCompare;

use std::fmt::Debug;
use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use crate::config::NvmRegion;
use crate::flashing::{FlashError, FlashLayout, FlashProgress, ProgressOperation};

/// One erase sector, and the content that it must hold.
#[derive(Debug)]
pub struct SectorCheck<'a> {
    /// The address range of the erase sector.
    pub sector: Range<u64>,
    /// The image data in the sector, as `(address, data)` pairs in ascending address order.
    pub expected: Vec<(u64, &'a [u8])>,
    /// The content of the bytes that `expected` does not cover.
    pub outside: Outside,
    /// The result. `None` means that no backend has one yet.
    pub outcome: Option<CompareOutcome>,
    /// The bytes that this check adds to the progress.
    progress_size: u64,
}

impl<'a> SectorCheck<'a> {
    /// Creates a check without a result.
    pub fn new(sector: Range<u64>, expected: Vec<(u64, &'a [u8])>, outside: Outside) -> Self {
        Self {
            sector,
            expected,
            outside,
            outcome: None,
            progress_size: 0,
        }
    }

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

    /// Returns the content to check, with adjacent ranges joined.
    pub(crate) fn contiguous_ranges(&self) -> Vec<(u64, Vec<u8>)> {
        if let Some(content) = self.sector_content() {
            return vec![(self.sector.start, content)];
        }

        let mut ranges: Vec<(u64, Vec<u8>)> = Vec::new();
        for &(address, data) in &self.expected {
            match ranges.last_mut() {
                Some((start, joined)) if *start + joined.len() as u64 == address => {
                    joined.extend_from_slice(data);
                }
                _ => ranges.push((address, data.to_vec())),
            }
        }

        ranges
    }

    /// Compares the flash contents that `read` reads back with this check.
    pub(crate) fn compare_read_back<E>(
        &self,
        mut read: impl FnMut(u64, &mut [u8]) -> Result<(), E>,
    ) -> Result<CompareOutcome, E> {
        if let Some(content) = self.sector_content() {
            let Outside::Erased(erased) = self.outside else {
                unreachable!("`sector_content` returns `Some` only for `Outside::Erased`")
            };

            let mut read_back = vec![0; content.len()];
            read(self.sector.start, &mut read_back)?;

            let outcome = if read_back == content {
                CompareOutcome::Match
            } else if read_back.iter().all(|&byte| byte == erased) {
                CompareOutcome::Empty
            } else {
                CompareOutcome::Mismatch
            };
            return Ok(outcome);
        }

        for &(address, data) in &self.expected {
            let mut read_back = vec![0; data.len()];
            read(address, &mut read_back)?;
            if read_back != data {
                return Ok(CompareOutcome::Mismatch);
            }
        }

        Ok(CompareOutcome::Match)
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
    /// The sector is erased, and the expected content is not. It needs no erase.
    Empty,
}

/// How much data a backend moves over the debug link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareCost {
    /// The backend moves signatures. It gets each run of adjacent sectors in one call.
    Cheap,
    /// The backend moves the sector data. It gets one sector in each call.
    Costly,
}

/// A method to compare flash sectors with their expected content.
///
/// Rules for an implementation:
///
/// - Never give [`CompareOutcome::Match`] or [`CompareOutcome::Empty`] when it is not true.
/// - [`CompareOutcome::Mismatch`] in place of [`CompareOutcome::Empty`] is correct.
/// - Leave [`SectorCheck::outcome`] at `None` for a sector that this method cannot check, so that
///   the next method gets it. Return errors.
/// - Only set [`SectorCheck::outcome`]. Other backends can use the rest of the check.
/// - Do not read erased flash on a target where such a read faults.
pub trait FlashCompare: Send + Sync + Debug {
    /// Returns how much data this method moves over the debug link.
    fn cost(&self) -> CompareCost;

    /// Sets the result of each check.
    ///
    /// Each check holds `None` on entry. All checks are in `region`.
    fn compare(
        &self,
        context: &mut CompareContext<'_>,
        region: &NvmRegion,
        checks: &mut [SectorCheck<'_>],
    ) -> Result<(), FlashError>;
}

/// Builds one check for each sector of `layout`.
///
/// If `ignore_fills` is `true`, the fill bytes are not part of the expected data.
fn sector_checks(
    layout: &FlashLayout,
    outside: Outside,
    ignore_fills: bool,
) -> impl Iterator<Item = SectorCheck<'_>> {
    let pages = layout.pages();
    // Sectors and pages are in address order, so each sector starts at the first page that
    // does not end before it.
    let mut first = 0;

    layout.sectors_to_check().into_iter().map(move |sector| {
        let range = sector.address()..sector.address() + sector.size();

        while pages
            .get(first)
            .is_some_and(|page| page.address() + page.size() as u64 <= range.start)
        {
            first += 1;
        }

        let mut expected: Vec<(u64, &[u8])> = Vec::new();
        let mut progress_size = 0;
        for (idx, page) in pages.iter().enumerate().skip(first) {
            if range.end <= page.address() {
                break;
            }

            let page_end = page.address() + page.size() as u64;
            progress_size += page_end.min(range.end) - page.address().max(range.start);

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
            outcome: None,
            progress_size,
        }
    })
}

/// Runs each backend of `chain` on each region.
///
/// Returns one result for each sector, for each region.
pub(crate) fn run_chain(
    chain: &[Arc<dyn FlashCompare>],
    mut regions: Vec<RegionCompare<'_>>,
    mut context: CompareContext<'_>,
    progress: &mut FlashProgress<'_>,
    operation: ProgressOperation,
) -> Result<Vec<Vec<CompareOutcome>>, FlashError> {
    for backend in chain {
        for region in &mut regions {
            region.run(backend.as_ref(), &mut context, progress, operation)?;
        }
    }
    context.finish()?;

    Ok(regions.into_iter().map(RegionCompare::finish).collect())
}

/// The checks of one region, while the backends run.
pub(crate) struct RegionCompare<'a> {
    region: &'a NvmRegion,
    checks: Vec<SectorCheck<'a>>,
}

impl<'a> RegionCompare<'a> {
    /// Builds the checks of `layout`.
    ///
    /// If `ignore_fills` is `true`, the fill bytes are not part of the expected data.
    pub(crate) fn new(
        region: &'a NvmRegion,
        layout: &'a FlashLayout,
        outside: Outside,
        ignore_fills: bool,
    ) -> Self {
        Self {
            region,
            checks: sector_checks(layout, outside, ignore_fills).collect(),
        }
    }

    /// Runs one backend on the checks that have no result yet.
    pub(crate) fn run(
        &mut self,
        backend: &dyn FlashCompare,
        context: &mut CompareContext<'_>,
        progress: &mut FlashProgress<'_>,
        operation: ProgressOperation,
    ) -> Result<(), FlashError> {
        let region = self.region;
        self.for_each_batch(backend.cost(), |batch| {
            let started = Instant::now();
            backend.compare(context, region, batch)?;

            for check in batch.iter() {
                tracing::debug!(
                    "{backend:?}: sector {:#010x?}: {:?}",
                    check.sector,
                    check.outcome
                );
            }

            let done = batch
                .iter()
                .filter(|check| check.outcome.is_some())
                .map(|check| check.progress_size)
                .sum::<u64>();
            if done > 0 {
                progress.progressed(operation, done, started.elapsed());
            }

            Ok(())
        })
    }

    /// Calls `compare` for the checks that have no result yet.
    ///
    /// Each batch holds adjacent sectors. A cheap backend gets each run of adjacent sectors in one
    /// batch. A costly backend gets one sector in each batch, so that the progress moves while it
    /// runs.
    fn for_each_batch(
        &mut self,
        cost: CompareCost,
        mut compare: impl FnMut(&mut [SectorCheck<'a>]) -> Result<(), FlashError>,
    ) -> Result<(), FlashError> {
        for run in self
            .checks
            .chunk_by_mut(|a, b| a.outcome.is_none() == b.outcome.is_none())
        {
            if run[0].outcome.is_some() {
                continue;
            }

            let batch_size = match cost {
                CompareCost::Cheap => run.len(),
                CompareCost::Costly => 1,
            };
            for batch in run.chunks_mut(batch_size) {
                compare(batch)?;
            }
        }

        Ok(())
    }

    /// Returns the results. A check that no backend could do counts as a mismatch.
    pub(crate) fn finish(self) -> Vec<CompareOutcome> {
        self.checks
            .iter()
            .map(|check| check.outcome.unwrap_or(CompareOutcome::Mismatch))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use probe_rs_target::{FlashProperties, SectorDescription};

    use super::*;
    use crate::flashing::FlashBuilder;

    const ERASED: u8 = 0xFF;

    fn test_region(range: Range<u64>) -> NvmRegion {
        NvmRegion {
            name: None,
            access: None,
            range,
            cores: vec!["main".into()],
            memory_ports: vec![],
            is_alias: false,
        }
    }

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
        let region = test_region(range);

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

        let checks = sector_checks(&layout, Outside::Any, true).collect::<Vec<_>>();

        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].sector, 0x0000..0x1000);
        assert_eq!(checks[0].expected, vec![(0x10, &[1, 2][..])]);
        assert_eq!(checks[1].sector, 0x1000..0x2000);
        assert_eq!(checks[1].expected, vec![(0x1400, &[3][..])]);
    }

    #[test]
    fn checks_after_a_prune_cover_the_programmed_sectors() {
        let mut layout = layout(&[(0x10, &[1]), (0x1010, &[2]), (0x2010, &[3])], false);
        layout.prune(&[
            CompareOutcome::Empty,
            CompareOutcome::Match,
            CompareOutcome::Mismatch,
        ]);

        let sectors = sector_checks(&layout, Outside::Any, false)
            .map(|check| check.sector)
            .collect::<Vec<_>>();

        assert_eq!(sectors, vec![0x0000..0x1000, 0x2000..0x3000]);
    }

    #[test]
    fn checks_split_pages_at_sector_boundaries() {
        let data = [0x5a; 128];
        let layout = layout_with_geometry(&[(0x6000, &data)], false, 0x8000, 0x2000);

        let checks = sector_checks(&layout, Outside::Any, true).collect::<Vec<_>>();

        assert_eq!(checks.len(), 4);
        assert!(checks[0].expected.is_empty());
        assert_eq!(checks[3].expected, vec![(0x6000, data.as_slice())]);

        let progress_sizes = checks.iter().map(|c| c.progress_size).collect::<Vec<_>>();
        assert_eq!(progress_sizes, vec![0x2000; 4]);
    }

    #[test]
    fn progress_size_counts_the_pages_of_each_sector() {
        let layout = layout(&[(0x0410, &[1]), (0x0c10, &[2]), (0x1010, &[3])], false);

        let checks = sector_checks(&layout, Outside::Any, true).collect::<Vec<_>>();

        let progress_sizes = checks.iter().map(|c| c.progress_size).collect::<Vec<_>>();
        assert_eq!(progress_sizes, vec![0x800, 0x400]);
    }

    /// Returns the sector addresses of each batch. `settle` gives the result of each sector.
    fn batches(
        compare: &mut RegionCompare<'_>,
        cost: CompareCost,
        settle: impl Fn(u64) -> Option<CompareOutcome>,
    ) -> Vec<Vec<u64>> {
        let mut batches = Vec::new();
        compare
            .for_each_batch(cost, |batch| {
                batches.push(batch.iter().map(|check| check.sector.start).collect());
                for check in batch {
                    check.outcome = settle(check.sector.start);
                }
                Ok(())
            })
            .unwrap();
        batches
    }

    #[test]
    fn declined_sectors_go_to_the_next_backend() {
        let layout = layout(
            &[
                (0x0000, &[1]),
                (0x1000, &[2]),
                (0x2000, &[3]),
                (0x3000, &[4]),
            ],
            false,
        );
        let region = test_region(0..0x4000);
        let mut compare = RegionCompare::new(&region, &layout, Outside::Any, true);

        // The first backend answers for two sectors, which splits the rest into two runs.
        let settled = |address| (address % 0x2000 == 0).then_some(CompareOutcome::Match);
        assert_eq!(
            batches(&mut compare, CompareCost::Cheap, settled),
            vec![vec![0x0000, 0x1000, 0x2000, 0x3000]]
        );

        // The second backend declines all of them.
        assert_eq!(
            batches(&mut compare, CompareCost::Cheap, |_| None),
            vec![vec![0x1000], vec![0x3000]]
        );

        // No backend had a result for these two sectors.
        assert_eq!(
            compare.finish(),
            vec![
                CompareOutcome::Match,
                CompareOutcome::Mismatch,
                CompareOutcome::Match,
                CompareOutcome::Mismatch,
            ]
        );
    }

    #[test]
    fn a_costly_backend_gets_one_sector_in_each_batch() {
        let layout = layout(&[(0x0000, &[1]), (0x1000, &[2])], false);
        let region = test_region(0..0x4000);
        let mut compare = RegionCompare::new(&region, &layout, Outside::Any, true);

        assert_eq!(
            batches(&mut compare, CompareCost::Costly, |_| None),
            vec![vec![0x0000], vec![0x1000]]
        );
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
    fn contiguous_ranges_join_adjacent_pages() {
        let layout = layout(&[(0x10, &[1; 0x800]), (0x0c00, &[2])], false);

        let checks = sector_checks(&layout, Outside::Any, false).collect::<Vec<_>>();

        let ranges = checks[0].contiguous_ranges();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges[0].1.len(), 0x1000);
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
        let checks = sector_checks(&layout, Outside::Erased(ERASED), true).collect::<Vec<_>>();

        let mut flash = vec![ERASED; 0x1000];
        assert_eq!(
            checks[0].compare_read_back(read_from(&flash)),
            Ok(CompareOutcome::Empty)
        );

        flash[0x10..0x12].copy_from_slice(&[1, 2]);
        assert_eq!(
            checks[0].compare_read_back(read_from(&flash)),
            Ok(CompareOutcome::Match)
        );

        // An old byte outside the image needs an erase.
        flash[0x800] = 0;
        assert_eq!(
            checks[0].compare_read_back(read_from(&flash)),
            Ok(CompareOutcome::Mismatch)
        );
    }

    #[test]
    fn read_back_with_any_outside_ignores_other_bytes() {
        let layout = layout(&[(0x10, &[1, 2])], false);
        let checks = sector_checks(&layout, Outside::Any, true).collect::<Vec<_>>();

        let mut flash = vec![0; 0x1000];
        flash[0x10..0x12].copy_from_slice(&[1, 2]);
        assert_eq!(
            checks[0].compare_read_back(read_from(&flash)),
            Ok(CompareOutcome::Match)
        );

        flash[0x11] = 0;
        assert_eq!(
            checks[0].compare_read_back(read_from(&flash)),
            Ok(CompareOutcome::Mismatch)
        );
    }
}
