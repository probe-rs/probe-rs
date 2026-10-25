//! Host-side flash programming implementation.
//!
//! This module provides flash programming via the host PC using debug interface
//! commands, rather than loading a flash algorithm into target RAM.

use std::sync::Arc;
use std::time::Instant;

use probe_rs_target::{FlashProperties, NvmRegion, RawFlashAlgorithm, SectorDescription};

use super::builder::FlashBuilder;
use super::{FlashError, FlashLayout, FlashProgress};
use crate::flashing::compare::{
    CompareCost, CompareOutcome, CompareState, Outside, ReadBackCore, build_checks, run_backend,
    split_by_region,
};
use crate::flashing::flasher::FlashData;
use crate::flashing::host_sequence::DebugFlashSequence;
use crate::flashing::progress::ProgressOperation;
use crate::memory::MemoryInterface;
use crate::session::Session;

/// Build a region-specific `FlashProperties` from an algorithm's full properties.
///
/// The algorithm's `flash_properties` covers the entire address space of all flash regions which might be noncontiguous.
/// When building a layout for a single `NvmRegion` we need
/// properties scoped to just that region so the sector iterator doesn't traverse phantom
/// sectors across the large gap between MAIN flash and other flash regions
fn region_flash_props(
    algo_props: &FlashProperties,
    region_range: &std::ops::Range<u64>,
) -> FlashProperties {
    let offset = region_range
        .start
        .saturating_sub(algo_props.address_range.start);

    // Find the sector descriptor whose address is <= the region offset (last such entry).
    let sector = algo_props
        .sectors
        .iter()
        .rfind(|s| s.address <= offset)
        .cloned()
        .unwrap_or_else(|| algo_props.sectors[0]);

    FlashProperties {
        address_range: region_range.clone(),
        page_size: algo_props.page_size,
        erased_byte_value: algo_props.erased_byte_value,
        program_page_timeout: algo_props.program_page_timeout,
        erase_sector_timeout: algo_props.erase_sector_timeout,
        // Single sector descriptor with offset 0 (relative to region start).
        sectors: vec![SectorDescription {
            size: sector.size,
            address: 0,
        }],
    }
}

/// A region loaded for host-side flash programming.
pub(super) struct HostLoadedRegion {
    /// The memory region being programmed.
    #[allow(dead_code)] // May be used for error reporting or debugging
    pub region: NvmRegion,
    /// The flash data to program.
    pub data: FlashData,
}

impl HostLoadedRegion {
    /// Returns the flash layout for this region.
    pub fn flash_layout(&self) -> &FlashLayout {
        self.data.layout()
    }
}

/// A flasher that uses host-side programming via debug interface commands.
///
/// Instead of loading a flash algorithm into target RAM and executing it,
/// this flasher sends commands directly to the device via the debug interface.
/// This is used for devices like TI CC23xx/CC27xx that support SACI commands
/// for flash programming.
pub struct HostSideFlasher {
    /// The debug flash sequence implementation.
    flash_sequence: Arc<dyn DebugFlashSequence>,
    /// The core index to use.
    pub(super) core_index: usize,
    /// The raw flash algorithm (for metadata like name).
    pub(super) flash_algorithm: RawFlashAlgorithm,
    /// Regions to program.
    pub(super) regions: Vec<HostLoadedRegion>,
}

impl HostSideFlasher {
    /// Create a new host-side flasher.
    pub fn new(
        flash_sequence: Arc<dyn DebugFlashSequence>,
        core_index: usize,
        raw_flash_algorithm: RawFlashAlgorithm,
    ) -> Self {
        Self {
            flash_sequence,
            core_index,
            flash_algorithm: raw_flash_algorithm,
            regions: Vec::new(),
        }
    }

    /// Add a region to be programmed.
    pub(crate) fn add_region(
        &mut self,
        region: NvmRegion,
        builder: &FlashBuilder,
        restore_unwritten_bytes: bool,
    ) -> Result<(), FlashError> {
        // Build region-specific flash properties from the algorithm's YAML-defined
        // flash_properties, scoped to exactly this NvmRegion.
        let flash_props = region_flash_props(&self.flash_algorithm.flash_properties, &region.range);
        let layout = builder.build_sectors_and_pages_from_properties(
            &region,
            &flash_props,
            restore_unwritten_bytes,
        )?;

        self.regions.push(HostLoadedRegion {
            region,
            data: FlashData::Raw(layout),
        });
        Ok(())
    }

    /// Returns the name of the flash algorithm.
    pub fn algorithm_name(&self) -> &str {
        &self.flash_algorithm.name
    }

    /// Host-side flashers don't support double buffering.
    ///
    /// Double buffering is a RAM-algorithm optimisation (two page buffers in target RAM so
    /// programming and data transfer can overlap).  It has no meaning for host-side flash
    /// where there is no RAM algorithm.
    pub(super) fn double_buffering_supported(&self) -> bool {
        false
    }

    /// Returns whether chip erase is supported, as reported by the flash sequence.
    ///
    /// Delegates to [`DebugFlashSequence::supports_chip_erase`] so each device can
    /// express its own capability.  Devices that handle erase internally (e.g. a
    /// toolbox that performs erase as part of factory programming) should return `false`
    /// to prevent the loader issuing a redundant separate erase step.
    pub(super) fn is_chip_erase_supported(&self, _session: &Session) -> bool {
        self.flash_sequence.supports_chip_erase()
    }

    /// Run chip erase via the debug flash sequence.
    pub(super) fn run_erase_all(
        &mut self,
        session: &mut Session,
        progress: &mut FlashProgress<'_>,
    ) -> Result<(), FlashError> {
        self.run_with_flash(session, |this, session| {
            tracing::info!("Host-side: Running chip erase");

            this.flash_sequence
                .erase_all(session)
                .map_err(|e| FlashError::ChipEraseFailed {
                    source: Box::new(e),
                })?;

            progress.finished_erasing();
            Ok(())
        })
    }

    fn run_with_flash<R>(
        &mut self,
        session: &mut Session,
        f: impl FnOnce(&mut Self, &mut Session) -> Result<R, FlashError>,
    ) -> Result<R, FlashError> {
        // Allow the sequence to perform any required setup (e.g. enter a special
        // programming mode or release the probe for an external toolbox).
        self.flash_sequence
            .prepare_flash(session)
            .map_err(FlashError::Core)?;

        let result = f(self, session);

        // Allow the sequence to perform end-of-flash cleanup (e.g., exit SACI mode
        // and reset the device so subsequent debug sessions work normally).
        self.flash_sequence
            .finish_flash(session)
            .map_err(FlashError::Core)?;

        result
    }

    /// Read back bytes that will be erased but are not part of the input data.
    fn fill_unwritten(
        &mut self,
        session: &mut Session,
        progress: &mut FlashProgress<'_>,
    ) -> Result<(), FlashError> {
        progress.started_filling();

        let result = (|| {
            let mut core = session.core(self.core_index).map_err(FlashError::Core)?;

            for region in &mut self.regions {
                let fills: Vec<_> = region
                    .flash_layout()
                    .fills()
                    .iter()
                    .map(|fill| (fill.address(), fill.size(), fill.page_index()))
                    .collect();

                for (address, size, page_index) in fills {
                    let start = Instant::now();
                    let layout = region.data.layout_mut();
                    let page = &mut layout.pages[page_index];
                    let page_offset = (address - page.address()) as usize;
                    let page_slice = &mut page.data_mut()[page_offset..][..size as usize];

                    core.read(address, page_slice).map_err(FlashError::Core)?;
                    progress.page_filled(size, start.elapsed());
                }
            }

            Ok(())
        })();

        match result {
            Ok(()) => {
                progress.finished_filling();
                Ok(())
            }
            Err(error) => {
                progress.failed_filling();
                Err(error)
            }
        }
    }

    /// Program flash via the debug flash sequence.
    pub(super) fn program(
        &mut self,
        session: &mut Session,
        progress: &mut FlashProgress<'_>,
        restore_unwritten_bytes: bool,
        _enable_double_buffering: bool,
        skip_erasing: bool,
        verify: bool,
    ) -> Result<(), FlashError> {
        tracing::debug!("Host-side: Starting program procedure");

        self.run_with_flash(session, |this, session| {
            if restore_unwritten_bytes {
                this.fill_unwritten(session, progress)?;
            }

            // If sector erase is not supported, fall back to chip erase once before programming.
            if !skip_erasing && !this.flash_sequence.supports_sector_erase() {
                tracing::info!("Host-side: Device does not support sector erase, using chip erase");
                this.flash_sequence.erase_all(session).map_err(|e| {
                    FlashError::ChipEraseFailed {
                        source: Box::new(e),
                    }
                })?;
                progress.finished_erasing();
            }

            // Check whether the sequence supports whole-image programming.  If so,
            // collect all (region, layout) pairs and call program_image() once instead
            // of the per-page loop below.
            let region_layouts: Vec<(&NvmRegion, &FlashLayout)> = this
                .regions
                .iter()
                .map(|r| (&r.region, r.flash_layout()))
                .collect();

            if this
                .flash_sequence
                .program_image(session, &region_layouts)
                .map_err(FlashError::Core)?
            {
                // Progress reporting for whole-image: report all pages as programmed.
                for r in &this.regions {
                    let layout = r.flash_layout();
                    for page in layout.pages() {
                        progress.page_programmed(page.size() as u64, std::time::Duration::ZERO);
                    }
                    progress.finished_programming();
                }

                if verify && !this.verify_regions(session, progress)? {
                    return Err(FlashError::Verify);
                }
            } else {
                // Process each region with the per-page loop.
                for region in &this.regions {
                    let layout = region.flash_layout();

                    // Erase sectors if not skipping and sector erase is supported.
                    if !skip_erasing && this.flash_sequence.supports_sector_erase() {
                        tracing::debug!("Host-side: Erasing sectors");
                        for sector in layout.sectors() {
                            tracing::debug!(
                                "Host-side: Erasing sector at 0x{:08X} ({} bytes)",
                                sector.address(),
                                sector.size()
                            );
                            this.flash_sequence
                                .erase_sector(session, sector.address())
                                .map_err(|e| FlashError::EraseFailed {
                                    sector_address: sector.address(),
                                    source: Box::new(e),
                                })?;
                        }
                        progress.finished_erasing();
                    }

                    // Program pages
                    tracing::debug!("Host-side: Programming pages");
                    let mut t = Instant::now();
                    for page in layout.pages() {
                        tracing::debug!(
                            "Host-side: Programming page at 0x{:08X} ({} bytes)",
                            page.address(),
                            page.data().len()
                        );
                        this.flash_sequence
                            .program(session, page.address(), page.data())
                            .map_err(|e| FlashError::PageWrite {
                                page_address: page.address(),
                                source: Box::new(e),
                            })?;

                        progress.page_programmed(page.size() as u64, t.elapsed());
                        t = Instant::now();
                    }
                    progress.finished_programming();
                }

                if verify && !this.verify_regions(session, progress)? {
                    return Err(FlashError::Verify);
                }
            }

            Ok(())
        })
    }

    /// Verify flash contents against expected data.
    ///
    /// Returns `true` if all pages verify successfully, `false` otherwise.
    pub(super) fn verify(
        &mut self,
        session: &mut Session,
        progress: &mut FlashProgress<'_>,
        _ignore_filled: bool,
    ) -> Result<bool, FlashError> {
        tracing::debug!("Host-side: Starting verify procedure");
        self.run_with_flash(session, |this, session| {
            this.verify_regions(session, progress)
        })
    }

    /// Checks each sector of each region. The fill bytes of a page must hold the page data.
    fn verify_regions(
        &self,
        session: &mut Session,
        progress: &mut FlashProgress<'_>,
    ) -> Result<bool, FlashError> {
        progress.started_verifying();

        let result = self
            .compare(
                session,
                progress,
                ProgressOperation::Verify,
                Outside::Any,
                false,
                true,
            )
            .map(|outcomes| {
                outcomes
                    .iter()
                    .flatten()
                    .all(|outcome| *outcome == CompareOutcome::Match)
            });

        match result.is_ok() {
            true => progress.finished_verifying(),
            false => progress.failed_verifying(),
        }

        result
    }

    /// Returns `true` if a compare can skip sectors.
    ///
    /// A costly compare backend counts only if `allow_costly` is `true`.
    pub(super) fn can_compare(&self, session: &Session, allow_costly: bool) -> bool {
        self.flash_sequence.supports_sector_erase()
            && (allow_costly
                || session
                    .target()
                    .debug_sequence
                    .flash_compare()
                    .is_some_and(|backend| backend.cost() == CompareCost::Cheap))
    }

    /// Compares the flash with the image, and removes the sectors that need no change.
    ///
    /// A costly compare backend runs only if `allow_costly` is `true`.
    pub(super) fn compare_and_prune(
        &mut self,
        session: &mut Session,
        progress: &mut FlashProgress<'_>,
        keep_unwritten_bytes: bool,
        allow_costly: bool,
    ) -> Result<(), FlashError> {
        // Without restored bytes, a full flash leaves the erased value outside the image.
        let outside = if keep_unwritten_bytes {
            Outside::Any
        } else {
            Outside::Erased(self.flash_algorithm.flash_properties.erased_byte_value)
        };

        progress.started(ProgressOperation::Compare);
        let result = self.run_with_flash(session, |this, session| {
            this.compare(
                session,
                progress,
                ProgressOperation::Compare,
                outside,
                true,
                allow_costly,
            )
        });
        match result.is_ok() {
            true => progress.finished(ProgressOperation::Compare),
            false => progress.failed(ProgressOperation::Compare),
        }

        for (region, outcomes) in self.regions.iter_mut().zip(result?) {
            region.data.layout_mut().prune(&outcomes);
        }

        Ok(())
    }

    /// Compares each sector of each region with its expected content.
    ///
    /// Returns one result for each sector, for each region. A sector that no backend can
    /// check is a mismatch. A costly backend runs only if `allow_costly` is `true`.
    pub(super) fn compare(
        &self,
        session: &mut Session,
        progress: &mut FlashProgress<'_>,
        operation: ProgressOperation,
        outside: Outside,
        ignore_fills: bool,
        allow_costly: bool,
    ) -> Result<Vec<Vec<CompareOutcome>>, FlashError> {
        let regions = self
            .regions
            .iter()
            .map(|region| region.region.clone())
            .collect::<Vec<_>>();
        let checks = build_checks(
            &self
                .regions
                .iter()
                .map(|region| region.flash_layout())
                .collect::<Vec<_>>(),
            outside,
            ignore_fills,
        );
        let mut state = CompareState::new(checks.len());

        let mut chain = session
            .target()
            .debug_sequence
            .flash_compare()
            .into_iter()
            .collect::<Vec<_>>();
        chain.push(Arc::new(ReadBackCore));

        for backend in chain {
            if backend.cost() == CompareCost::Costly && !allow_costly {
                continue;
            }
            run_backend(
                &backend,
                &regions,
                &checks,
                &mut state,
                backend.cost(),
                progress,
                operation,
                |region, batch, out| {
                    backend
                        .compare(session, self.core_index, region, batch, out)
                        .map_err(FlashError::Core)
                },
            )?;
        }

        Ok(split_by_region(&checks, state.finish(), regions.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_flash_props_preserves_program_page_size() {
        let properties = FlashProperties {
            address_range: 0x0800_0000..0x0810_0000,
            page_size: 32,
            erased_byte_value: 0xff,
            program_page_timeout: 100,
            erase_sector_timeout: 30_000,
            sectors: vec![SectorDescription {
                size: 0x1000,
                address: 0,
            }],
        };

        let scoped = region_flash_props(&properties, &(0x0800_1000..0x0800_3000));

        assert_eq!(scoped.page_size, 32);
        assert_eq!(scoped.sectors[0].size, 0x1000);
        assert_eq!(scoped.sectors[0].address, 0);
    }
}
