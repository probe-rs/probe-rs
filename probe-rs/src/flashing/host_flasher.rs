//! Host-side flash programming implementation.
//!
//! This module provides flash programming via the host PC using debug interface
//! commands, rather than loading a flash algorithm into target RAM.

use std::sync::Arc;
use std::time::Instant;

use probe_rs_target::{FlashProperties, NvmRegion, RawFlashAlgorithm, SectorDescription};

use super::builder::FlashBuilder;
use super::{FlashError, FlashLayout, FlashProgress};
use crate::flashing::flasher::FlashData;
use crate::flashing::host_sequence::DebugFlashSequence;
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
        page_size: sector.size as u32,
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
        tracing::info!("Host-side: Running chip erase");

        self.flash_sequence
            .erase_all(session)
            .map_err(|e| FlashError::ChipEraseFailed {
                source: Box::new(e),
            })?;

        progress.finished_erasing();
        Ok(())
    }

    /// Program flash via the debug flash sequence.
    pub(super) fn program(
        &mut self,
        session: &mut Session,
        progress: &mut FlashProgress<'_>,
        _restore_unwritten_bytes: bool,
        _enable_double_buffering: bool,
        skip_erasing: bool,
        verify: bool,
    ) -> Result<(), FlashError> {
        tracing::debug!("Host-side: Starting program procedure");

        // Allow the sequence to perform any required setup (e.g. enter a special
        // programming mode or release the probe for an external toolbox).
        self.flash_sequence
            .prepare_flash(session)
            .map_err(FlashError::Core)?;

        // If sector erase is not supported, fall back to chip erase once before programming.
        if !skip_erasing && !self.flash_sequence.supports_sector_erase() {
            tracing::info!("Host-side: Device does not support sector erase, using chip erase");
            self.flash_sequence
                .erase_all(session)
                .map_err(|e| FlashError::ChipEraseFailed {
                    source: Box::new(e),
                })?;
            progress.finished_erasing();
        }

        // Check whether the sequence supports whole-image programming.  If so,
        // collect all (region, layout) pairs and call program_image() once instead
        // of the per-page loop below.
        let region_layouts: Vec<(&NvmRegion, &FlashLayout)> = self
            .regions
            .iter()
            .map(|r| (&r.region, r.flash_layout()))
            .collect();

        if let Some(result) = self.flash_sequence.program_image(session, &region_layouts) {
            result.map_err(FlashError::Core)?;
            // Progress reporting for whole-image: report all pages as programmed.
            for r in &self.regions {
                let layout = r.flash_layout();
                for page in layout.pages() {
                    progress.page_programmed(page.size() as u64, std::time::Duration::ZERO);
                }
                progress.finished_programming();
            }
        } else {
            // Process each region with the per-page loop.
            for region in &self.regions {
                let layout = region.flash_layout();

                // Erase sectors if not skipping and sector erase is supported.
                if !skip_erasing && self.flash_sequence.supports_sector_erase() {
                    tracing::debug!("Host-side: Erasing sectors");
                    for sector in layout.sectors() {
                        tracing::debug!(
                            "Host-side: Erasing sector at 0x{:08X} ({} bytes)",
                            sector.address(),
                            sector.size()
                        );
                        self.flash_sequence
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
                    self.flash_sequence
                        .program(session, page.address(), page.data())
                        .map_err(|e| FlashError::PageWrite {
                            page_address: page.address(),
                            source: Box::new(e),
                        })?;

                    progress.page_programmed(page.size() as u64, t.elapsed());
                    t = Instant::now();
                }
                progress.finished_programming();

                // Verify if requested
                if verify {
                    tracing::debug!("Host-side: Verifying");
                    for page in layout.pages() {
                        let verified = self
                            .flash_sequence
                            .verify(session, page.address(), page.data())
                            .map_err(FlashError::Core)?;

                        if !verified {
                            tracing::error!(
                                "Host-side: Verification failed at address 0x{:08X}",
                                page.address()
                            );
                            return Err(FlashError::Verify);
                        }
                    }
                }
            }
        }

        // Allow the sequence to perform end-of-flash cleanup (e.g., exit SACI mode
        // and reset the device so subsequent debug sessions work normally).
        self.flash_sequence
            .finish_flash(session)
            .map_err(FlashError::Core)?;

        Ok(())
    }

    /// Verify flash contents against expected data.
    ///
    /// Returns `true` if all pages verify successfully, `false` otherwise.
    pub(super) fn verify(
        &self,
        session: &mut Session,
        _progress: &mut FlashProgress<'_>,
        _ignore_filled: bool,
    ) -> Result<bool, FlashError> {
        tracing::debug!("Host-side: Starting verify procedure");

        // prepare_flash() is the single entry point for entering the required
        // mode — used here for the standalone verify pass the same way it is
        // used before the full erase+program path.
        self.flash_sequence
            .prepare_flash(session)
            .map_err(FlashError::Core)?;

        for region in &self.regions {
            let layout = region.flash_layout();
            for page in layout.pages() {
                let verified = self
                    .flash_sequence
                    .verify(session, page.address(), page.data())
                    .map_err(FlashError::Core)?;

                if !verified {
                    tracing::error!(
                        "Host-side: Verification failed at address 0x{:08X}",
                        page.address()
                    );
                    return Ok(false);
                }
            }
        }

        // Exit any special verification mode and leave the device in a clean state.
        self.flash_sequence
            .finish_flash(session)
            .map_err(FlashError::Core)?;

        Ok(true)
    }
}
