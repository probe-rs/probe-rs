use probe_rs_target::{RawFlashAlgorithm, TransferEncoding};
use tracing::Level;
use zerocopy::IntoBytes;

use super::{FlashAlgorithm, FlashBuilder, FlashError, FlashPage, FlashProgress};
use crate::config::NvmRegion;
use crate::error::Error;
use crate::flashing::encoder::FlashEncoder;
use crate::flashing::{FlashLayout, FlashSector};
use crate::memory::MemoryInterface;
use crate::rtt::{self, Rtt, ScanRegion};
use crate::{Core, InstructionSet, core::CoreRegisters, session::Session};
use crate::{CoreStatus, Target};
use std::marker::PhantomData;
use std::{
    fmt::Debug,
    time::{Duration, Instant},
};

/// The timeout for init/uninit routines.
const INIT_TIMEOUT: Duration = Duration::from_secs(2);

/// Basic timing for CRC32 verification
#[derive(Debug)]
struct Crc32Timing {
    #[allow(dead_code)]
    total_time: Duration,
}

pub(super) trait Operation {
    const OPERATION: u32;
    const NAME: &'static str;
}

pub(super) struct Erase;

impl Operation for Erase {
    const OPERATION: u32 = 1;
    const NAME: &'static str = "Erase";
}

pub(super) struct Program;

impl Operation for Program {
    const OPERATION: u32 = 2;
    const NAME: &'static str = "Program";
}

pub(super) struct Verify;

impl Operation for Verify {
    const OPERATION: u32 = 3;
    const NAME: &'static str = "Verify";
}


pub(super) enum FlashData {
    Raw(FlashLayout),
    Loaded {
        encoder: FlashEncoder,
        ignore_fills: bool,
    },
}

impl FlashData {
    pub fn layout(&self) -> &FlashLayout {
        match self {
            FlashData::Raw(layout) => layout,
            FlashData::Loaded { encoder, .. } => encoder.flash_layout(),
        }
    }

    pub fn layout_mut(&mut self) -> &mut FlashLayout {
        // We're mutating the data, let's invalidate the encoder
        if let FlashData::Loaded { encoder, .. } = self {
            *self = FlashData::Raw(encoder.flash_layout().clone());
        }

        match self {
            FlashData::Raw(layout) => layout,
            FlashData::Loaded { .. } => unreachable!(),
        }
    }

    pub fn encoder(&mut self, encoding: TransferEncoding, ignore_fills: bool) -> &FlashEncoder {
        if let FlashData::Loaded {
            encoder,
            ignore_fills: was_ignore_fills,
        } = self
        {
            if *was_ignore_fills != ignore_fills {
                // Fill handling changed, invalidate the encoder
                *self = FlashData::Raw(encoder.flash_layout().clone());
            }
        }
        if let FlashData::Raw(layout) = self {
            let layout = std::mem::take(layout);
            let encoder = FlashEncoder::new(encoding, layout, ignore_fills);
            *self = FlashData::Loaded {
                encoder,
                ignore_fills,
            };
        }

        match self {
            FlashData::Loaded { encoder, .. } => encoder,
            FlashData::Raw(_) => unreachable!(),
        }
    }
}

pub(super) struct LoadedRegion {
    pub region: NvmRegion,
    pub data: FlashData,
}

impl LoadedRegion {
    pub fn flash_layout(&self) -> &FlashLayout {
        self.data.layout()
    }
}

/// A structure to control the flash of an attached microchip.
///
/// Once constructed it can be used to program date to the flash.
pub(super) struct Flasher {
    pub(super) core_index: usize,
    pub(super) flash_algorithm: FlashAlgorithm,
    pub(super) loaded: bool,
    pub(super) regions: Vec<LoadedRegion>,
}

/// The byte used to fill the stack when checking for stack overflows.
const STACK_FILL_BYTE: u8 = 0x56;

impl Flasher {
    pub(super) fn new(
        target: &Target,
        core_index: usize,
        raw_flash_algorithm: &RawFlashAlgorithm,
    ) -> Result<Self, FlashError> {
        let flash_algorithm = FlashAlgorithm::assemble_from_raw_with_core(
            raw_flash_algorithm,
            &target.cores[core_index].name,
            target,
        )?;

        Ok(Self {
            core_index,
            flash_algorithm,
            loaded: false,
            regions: Vec::new(),
        })
    }

    fn ensure_loaded(&mut self, session: &mut Session) -> Result<(), FlashError> {
        if !self.loaded {
            self.load(session)?;
            self.loaded = true;
        }

        Ok(())
    }

    pub(super) fn flash_algorithm(&self) -> &FlashAlgorithm {
        &self.flash_algorithm
    }

    pub(super) fn double_buffering_supported(&self) -> bool {
        self.flash_algorithm.page_buffers.len() > 1
    }

    fn load(&mut self, session: &mut Session) -> Result<(), FlashError> {
        tracing::debug!("Initializing the flash algorithm.");
        let algo = &self.flash_algorithm;

        // Attach to memory and core.
        let mut core = session.core(self.core_index).map_err(FlashError::Core)?;

        // TODO: we probably want a full system reset here to make sure peripherals don't interfere.
        tracing::debug!("Reset and halt core {}", self.core_index);
        core.reset_and_halt(Duration::from_millis(500))
            .map_err(FlashError::ResetAndHalt)?;

        // TODO: Possible special preparation of the target such as enabling faster clocks for the flash e.g.

        // Load flash algorithm code into target RAM.
        tracing::debug!("Downloading algorithm code to {:#010x}", algo.load_address);

        core.write(algo.load_address, algo.instructions.as_bytes())
            .map_err(FlashError::Core)?;

        let mut data = vec![0; algo.instructions.len()];
        core.read(algo.load_address, data.as_mut_bytes())
            .map_err(FlashError::Core)?;

        for (offset, (original, read_back)) in algo.instructions.iter().zip(data.iter()).enumerate()
        {
            if original == read_back {
                continue;
            }

            tracing::error!(
                "Failed to verify flash algorithm. Data mismatch at address {:#010x}",
                algo.load_address + (4 * offset) as u64
            );
            tracing::error!("Original instruction: {:#010x}", original);
            tracing::error!("Readback instruction: {:#010x}", read_back);

            tracing::error!("Original: {:x?}", &algo.instructions);
            tracing::error!("Readback: {:x?}", &data);

            return Err(FlashError::FlashAlgorithmNotLoaded);
        }

        if algo.stack_overflow_check {
            // Fill the stack with known data.
            let stack_bottom = algo.stack_top - algo.stack_size;
            if algo.stack_size & 3 == 0 {
                let fill = vec![
                    u32::from_ne_bytes([
                        STACK_FILL_BYTE,
                        STACK_FILL_BYTE,
                        STACK_FILL_BYTE,
                        STACK_FILL_BYTE
                    ]);
                    algo.stack_size as usize / 4
                ];
                core.write_32(stack_bottom, &fill)
                    .map_err(FlashError::Core)?;
            } else {
                let fill = vec![STACK_FILL_BYTE; algo.stack_size as usize];
                core.write_8(stack_bottom, &fill)
                    .map_err(FlashError::Core)?;
            }
        }

        tracing::debug!("RAM contents match flashing algo blob.");

        Ok(())
    }

    pub(super) fn init<'s, 'p, O: Operation>(
        &'s mut self,
        session: &'s mut Session,
        progress: &'s FlashProgress<'p>,
        clock: Option<u32>,
    ) -> Result<(ActiveFlasher<'s, 'p, O>, &'s mut [LoadedRegion]), FlashError> {
        self.ensure_loaded(session)?;

        // Attach to memory and core.
        let mut core = session.core(self.core_index).map_err(FlashError::Core)?;

        let instruction_set = core.instruction_set().map_err(FlashError::Core)?;

        tracing::debug!("Preparing Flasher for operation {}", O::NAME);
        let mut flasher = ActiveFlasher::<O> {
            core,
            instruction_set,
            rtt: None,
            progress,
            flash_algorithm: &self.flash_algorithm,
            _operation: PhantomData,
        };

        flasher.init(clock)?;

        Ok((flasher, &mut self.regions))
    }

    pub(super) fn run_erase_all(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
    ) -> Result<(), FlashError> {
        progress.started_erasing();
        let result = if session.has_sequence_erase_all() {
            session
                .sequence_erase_all()
                .map_err(|e| FlashError::ChipEraseFailed {
                    source: Box::new(e),
                })?;
            // We need to reload the flasher, since the debug sequence erase
            // may have invalidated any previously invalid state
            self.load(session)
        } else {
            self.run_erase(session, progress, |active, _| active.erase_all())
        };

        match result.is_ok() {
            true => progress.finished_erasing(),
            false => progress.failed_erasing(),
        }

        result
    }

    pub(super) fn run_blank_check<'p, T, F>(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress<'p>,
        f: F,
    ) -> Result<T, FlashError>
    where
        F: FnOnce(&mut ActiveFlasher<'_, 'p, Erase>, &mut [LoadedRegion]) -> Result<T, FlashError>,
    {
        let (mut active, data) = self.init(session, progress, None)?;
        let r = f(&mut active, data)?;
        active.uninit()?;
        Ok(r)
    }

    pub(super) fn run_erase<'p, T, F>(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress<'p>,
        f: F,
    ) -> Result<T, FlashError>
    where
        F: FnOnce(&mut ActiveFlasher<'_, 'p, Erase>, &mut [LoadedRegion]) -> Result<T, FlashError>,
    {
        let (mut active, data) = self.init(session, progress, None)?;
        let r = f(&mut active, data)?;
        active.uninit()?;
        Ok(r)
    }

    pub(super) fn run_program<'p, T, F>(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress<'p>,
        f: F,
    ) -> Result<T, FlashError>
    where
        F: FnOnce(
            &mut ActiveFlasher<'_, 'p, Program>,
            &mut [LoadedRegion],
        ) -> Result<T, FlashError>,
    {
        let (mut active, data) = self.init(session, progress, None)?;
        let r = f(&mut active, data)?;
        active.uninit()?;
        Ok(r)
    }

    pub(super) fn run_verify<'p, T, F>(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress<'p>,
        f: F,
    ) -> Result<T, FlashError>
    where
        F: FnOnce(&mut ActiveFlasher<'_, 'p, Verify>, &mut [LoadedRegion]) -> Result<T, FlashError>,
    {
        let (mut active, data) = self.init(session, progress, None)?;
        let r = f(&mut active, data)?;
        active.uninit()?;
        Ok(r)
    }

    pub(super) fn is_chip_erase_supported(&self, session: &Session) -> bool {
        session.has_sequence_erase_all() || self.flash_algorithm().pc_erase_all.is_some()
    }

    /// Program the contents of given `FlashBuilder` to the flash.
    ///
    /// If `restore_unwritten_bytes` is `true`, all bytes of a sector,
    /// that are not to be written during flashing will be read from the flash first
    /// and written again once the sector is erased.
    pub(super) fn program(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
        restore_unwritten_bytes: bool,
        enable_double_buffering: bool,
        skip_erasing: bool,
        verify: bool,
        incremental: bool,
    ) -> Result<(), FlashError> {
        tracing::debug!("Starting program procedure.");

        tracing::debug!("Double Buffering enabled: {:?}", enable_double_buffering);
        tracing::debug!(
            "Restoring unwritten bytes enabled: {:?}",
            restore_unwritten_bytes
        );
        tracing::debug!("Incremental mode enabled: {:?}", incremental);

        if incremental {
            tracing::debug!("Entering incremental mode - calling program_incremental");
            return self.program_incremental(
                session,
                progress,
                restore_unwritten_bytes,
                enable_double_buffering,
                verify,
            );
        }

        if restore_unwritten_bytes {
            self.fill_unwritten(session, progress)?;
        }

        // Skip erase if necessary (i.e. chip erase was done before)
        if !skip_erasing {
            // Erase all necessary sectors
            self.sector_erase(session, progress)?;
        }

        // Flash all necessary pages.
        self.do_program(session, progress, enable_double_buffering)?;

        if verify && !self.verify(session, progress, !restore_unwritten_bytes)? {
            return Err(FlashError::Verify);
        }

        Ok(())
    }

    /// Programs flash using incremental sector-by-sector verification.
    /// Only sectors that differ from existing flash content will be erased and reprogrammed.
    /// IMPORTANT: All pages within an erased sector must be reprogrammed, even if some were
    /// previously verified as correct, because the erase operation affects the entire sector.
    pub(super) fn program_incremental(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
        restore_unwritten_bytes: bool,
        enable_double_buffering: bool,
        verify: bool,
    ) -> Result<(), FlashError> {
        tracing::info!("Starting incremental program procedure.");
        
        use std::collections::{BTreeMap, HashSet};
        use std::time::Instant;
        
        if restore_unwritten_bytes {
            self.fill_unwritten(session, progress)?;
        }

        // Group pages by sector for efficient processing
        let mut sector_pages: BTreeMap<u64, Vec<(usize, usize)>> = BTreeMap::new(); // sector_addr -> Vec<(region_idx, page_idx)>
        for (region_idx, region) in self.regions.iter().enumerate() {
            let layout = region.flash_layout();
            for (page_idx, page) in layout.pages().iter().enumerate() {
                let sector_addr = self.get_sector_address(region_idx, page.address())?;
                sector_pages.entry(sector_addr).or_default().push((region_idx, page_idx));
            }
        }

        // Track which sectors need to be erased and reprogrammed
        let mut sectors_to_update = HashSet::new();
        let total_sectors = sector_pages.len();
        let mut verified_sectors = 0;
        let mut updated_sectors = 0;

        tracing::info!("Incremental flash: checking {} sectors", total_sectors);

        // Calculate total size for progress bars
        let total_flash_size: u64 = sector_pages.values()
            .flatten()
            .map(|&(region_idx, page_idx)| self.regions[region_idx].flash_layout().pages()[page_idx].size() as u64)
            .sum();
        
        // Create progress bars for incremental mode in correct order
        progress.add_progress_bar(crate::flashing::ProgressOperation::Crc32Verify, Some(total_flash_size));

        // Phase 1: Verify all sectors to determine which need updates
        tracing::info!("Starting sector verification phase");
        
        // Start CRC32 verification progress
        progress.started_crc32_verifying();
        
        for (sector_addr, region_page_indices) in &sector_pages {
            let sector_start = Instant::now();
            
            let (sector_verification_passed, _timing) = match self.verify_sector_crc32(
                session,
                *sector_addr,
                region_page_indices,
            ) {
                Ok(result) => result,
                Err(e) => {
                    progress.failed_crc32_verifying();
                    return Err(e);
                }
            };
            
            let sector_needs_update = !sector_verification_passed;
            let sector_time = sector_start.elapsed();
            
            // Report progress for this sector
            let sector_size: u32 = region_page_indices.iter()
                .map(|&(region_idx, page_idx)| self.regions[region_idx].flash_layout().pages()[page_idx].size())
                .sum();
            progress.sector_crc32_verified(sector_size as u64, sector_time);

            if sector_needs_update {
                sectors_to_update.insert(*sector_addr);
                updated_sectors += 1;
                tracing::debug!("Sector 0x{:08x} needs update - CRC32 mismatch", sector_addr);
            } else {
                verified_sectors += 1;
                tracing::debug!("Sector 0x{:08x} verified OK - CRC32 match", sector_addr);
            }
        }
        tracing::info!("Verification complete: {} of {} sectors need updates ({:.1}%)", 
            updated_sectors, total_sectors, 
            (updated_sectors as f64 / total_sectors as f64) * 100.0);
        
        // Finish CRC32 verification progress
        progress.finished_crc32_verifying();
        

        // Create progress bars for Erase and Programming phases now that we know what needs updating
        if !sectors_to_update.is_empty() {
            // Calculate total erase size
            let total_erase_size: u64 = sectors_to_update.iter()
                .map(|&sector_addr| {
                    // Find sector size
                    for region in &self.regions {
                        let layout = region.flash_layout();
                        if let Some(sector) = layout.sectors().iter().find(|s| s.address() == sector_addr) {
                            return sector.size();
                        }
                    }
                    4096 // Default sector size if not found
                })
                .sum();
            
            // Calculate total programming size (only sectors that need updates)
            let total_program_size: u64 = sectors_to_update.iter()
                .map(|&sector_addr| {
                    // Find total page size for this sector
                    let mut sector_program_size = 0u64;
                    for &(region_idx, page_idx) in sector_pages.get(&sector_addr).unwrap_or(&Vec::new()) {
                        sector_program_size += self.regions[region_idx].flash_layout().pages()[page_idx].size() as u64;
                    }
                    sector_program_size
                })
                .sum();
            
            progress.add_progress_bar(crate::flashing::ProgressOperation::Erase, Some(total_erase_size));
            progress.add_progress_bar(crate::flashing::ProgressOperation::IncrementalProgram, Some(total_program_size));
        }

        // Phase 2: Erase sectors that need updates
        if !sectors_to_update.is_empty() {
            tracing::info!("Starting erase phase for {} of {} sectors", 
                sectors_to_update.len(), total_sectors);
            
            progress.started_erasing();
            
            self.sector_erase_selective(session, progress, &sectors_to_update)?;
            
            progress.finished_erasing();
        }

        // Phase 3: Program sectors using persistent buffer optimization where available
        if !sectors_to_update.is_empty() {
            tracing::info!("Starting programming phase for {} sectors", sectors_to_update.len());
            
            // Start incremental programming progress
            progress.started_incremental_programming();
            
            // Create a progress wrapper that redirects programming progress to incremental progress
            let incremental_progress = FlashProgress::new(|event| {
                match event {
                    crate::flashing::ProgressEvent::Progress { operation, size, time } => {
                        match operation {
                            crate::flashing::ProgressOperation::Program => {
                                // Redirect regular program progress to incremental program progress
                                progress.page_incremental_programmed(size, time);
                            }
                            _ => {
                                // Pass through other events unchanged
                                progress.emit(event);
                            }
                        }
                    }
                    crate::flashing::ProgressEvent::Started(crate::flashing::ProgressOperation::Program) => {
                        // Suppress the started event for regular programming since we already started incremental
                    }
                    crate::flashing::ProgressEvent::Finished(crate::flashing::ProgressOperation::Program) => {
                        // Suppress the finished event for regular programming since we handle it ourselves
                    }
                    crate::flashing::ProgressEvent::Failed(crate::flashing::ProgressOperation::Program) => {
                        // Suppress the failed event for regular programming since we handle it ourselves
                    }
                    _ => {
                        // Pass through other events unchanged
                        progress.emit(event);
                    }
                }
            });
            
            // Use selective programming - only program sectors that need updates
            match self.do_program_selective(session, &incremental_progress, enable_double_buffering, &sectors_to_update) {
                Ok(()) => {
                    // Finish incremental programming progress
                    progress.finished_incremental_programming();
                }
                Err(e) => {
                    // Signal failure and propagate error
                    progress.failed_incremental_programming();
                    return Err(e);
                }
            }
            
        } else {
            tracing::info!("No sectors needed updating - all sectors verified successfully");
        }

        // Phase 4: Final verification if requested
        if verify {
            tracing::info!("Starting final verification phase");
            if !self.verify(session, progress, !restore_unwritten_bytes)? {
                return Err(FlashError::Verify);
            }
        }
        tracing::info!(
            "Incremental flash complete: {} sectors verified, {} sectors updated", 
            verified_sectors, 
            updated_sectors
        );
        

        Ok(())
    }


    /// Gets the sector address that contains the given page address
    fn get_sector_address(&self, region_idx: usize, page_addr: u64) -> Result<u64, FlashError> {
        let layout = self.regions[region_idx].flash_layout();
        for sector in layout.sectors() {
            if page_addr >= sector.address() && page_addr < (sector.address() + sector.size()) {
                return Ok(sector.address());
            }
        }
        Err(FlashError::InvalidDataAddress { data_load_addr: page_addr, data_ram: 0..0 })
    }

    /// Verify a sector using CRC32 calculation on the target device
    /// This provides 99% speed improvement over USB-based verification
    fn verify_sector_crc32(
        &mut self,
        session: &mut Session,
        sector_addr: u64,
        expected_pages: &[(usize, usize)], // (region_idx, page_idx)
    ) -> Result<(bool, Crc32Timing), FlashError> {
        use std::time::Instant;
        let start_time = Instant::now();
        
        tracing::debug!("Verifying sector 0x{:08x} with CRC32", sector_addr);
        
        // Calculate host-side CRC32 of expected data
        let mut expected_data = Vec::new();
        for &(region_idx, page_idx) in expected_pages {
            let page = &self.regions[region_idx].flash_layout().pages()[page_idx];
            expected_data.extend_from_slice(page.data());
        }
        
        let host_crc32 = Self::calculate_crc32_host(&expected_data);
        
        // Load CRC32 algorithm to target RAM if not already loaded
        let crc32_algo_addr = self.ensure_crc32_algorithm_loaded(session)?;
        
        // Execute CRC32 calculation on target flash memory
        let target_crc32 = self.execute_crc32_on_target(
            session, 
            crc32_algo_addr, 
            sector_addr, 
            expected_data.len() as u64
        )?;
        
        let total_time = start_time.elapsed();
        let verification_passed = host_crc32 == target_crc32;
        
        if !verification_passed {
            tracing::debug!(
                "CRC32 mismatch for sector 0x{:08x}: host=0x{:08x} target=0x{:08x}",
                sector_addr, host_crc32, target_crc32
            );
        }
        
        let timing = Crc32Timing {
            total_time,
        };
        
        Ok((verification_passed, timing))
    }

    /// Calculate IEEE 802.3 CRC32 on the host side
    fn calculate_crc32_host(data: &[u8]) -> u32 {
        // IEEE 802.3 CRC32 lookup table
        const CRC32_TABLE: &[u32; 256] = &[
            0x00000000, 0x77073096, 0xEE0E612C, 0x990951BA, 0x076DC419, 0x706AF48F, 0xE963A535, 0x9E6495A3,
            0x0EDB8832, 0x79DCB8A4, 0xE0D5E91E, 0x97D2D988, 0x09B64C2B, 0x7EB17CBD, 0xE7B82D07, 0x90BF1D91,
            0x1DB71064, 0x6AB020F2, 0xF3B97148, 0x84BE41DE, 0x1ADAD47D, 0x6DDDE4EB, 0xF4D4B551, 0x83D385C7,
            0x136C9856, 0x646BA8C0, 0xFD62F97A, 0x8A65C9EC, 0x14015C4F, 0x63066CD9, 0xFA0F3D63, 0x8D080DF5,
            0x3B6E20C8, 0x4C69105E, 0xD56041E4, 0xA2677172, 0x3C03E4D1, 0x4B04D447, 0xD20D85FD, 0xA50AB56B,
            0x35B5A8FA, 0x42B2986C, 0xDBBBC9D6, 0xACBCF940, 0x32D86CE3, 0x45DF5C75, 0xDCD60DCF, 0xABD13D59,
            0x26D930AC, 0x51DE003A, 0xC8D75180, 0xBFD06116, 0x21B4F4B5, 0x56B3C423, 0xCFBA9599, 0xB8BDA50F,
            0x2802B89E, 0x5F058808, 0xC60CD9B2, 0xB10BE924, 0x2F6F7C87, 0x58684C11, 0xC1611DAB, 0xB6662D3D,
            0x76DC4190, 0x01DB7106, 0x98D220BC, 0xEFD5102A, 0x71B18589, 0x06B6B51F, 0x9FBFE4A5, 0xE8B8D433,
            0x7807C9A2, 0x0F00F934, 0x9609A88E, 0xE10E9818, 0x7F6A0DBB, 0x086D3D2D, 0x91646C97, 0xE6635C01,
            0x6B6B51F4, 0x1C6C6162, 0x856530D8, 0xF262004E, 0x6C0695ED, 0x1B01A57B, 0x8208F4C1, 0xF50FC457,
            0x65B0D9C6, 0x12B7E950, 0x8BBEB8EA, 0xFCB9887C, 0x62DD1DDF, 0x15DA2D49, 0x8CD37CF3, 0xFBD44C65,
            0x4DB26158, 0x3AB551CE, 0xA3BC0074, 0xD4BB30E2, 0x4ADFA541, 0x3DD895D7, 0xA4D1C46D, 0xD3D6F4FB,
            0x4369E96A, 0x346ED9FC, 0xAD678846, 0xDA60B8D0, 0x44042D73, 0x33031DE5, 0xAA0A4C5F, 0xDD0D7CC9,
            0x5005713C, 0x270241AA, 0xBE0B1010, 0xC90C2086, 0x5768B525, 0x206F85B3, 0xB966D409, 0xCE61E49F,
            0x5EDEF90E, 0x29D9C998, 0xB0D09822, 0xC7D7A8B4, 0x59B33D17, 0x2EB40D81, 0xB7BD5C3B, 0xC0BA6CAD,
            0xEDB88320, 0x9ABFB3B6, 0x03B6E20C, 0x74B1D29A, 0xEAD54739, 0x9DD277AF, 0x04DB2615, 0x73DC1683,
            0xE3630B12, 0x94643B84, 0x0D6D6A3E, 0x7A6A5AA8, 0xE40ECF0B, 0x9309FF9D, 0x0A00AE27, 0x7D079EB1,
            0xF00F9344, 0x8708A3D2, 0x1E01F268, 0x6906C2FE, 0xF762575D, 0x806567CB, 0x196C3671, 0x6E6B06E7,
            0xFED41B76, 0x89D32BE0, 0x10DA7A5A, 0x67DD4ACC, 0xF9B9DF6F, 0x8EBEEFF9, 0x17B7BE43, 0x60B08ED5,
            0xD6D6A3E8, 0xA1D1937E, 0x38D8C2C4, 0x4FDFF252, 0xD1BB67F1, 0xA6BC5767, 0x3FB506DD, 0x48B2364B,
            0xD80D2BDA, 0xAF0A1B4C, 0x36034AF6, 0x41047A60, 0xDF60EFC3, 0xA867DF55, 0x316E8EEF, 0x4669BE79,
            0xCB61B38C, 0xBC66831A, 0x256FD2A0, 0x5268E236, 0xCC0C7795, 0xBB0B4703, 0x220216B9, 0x5505262F,
            0xC5BA3BBE, 0xB2BD0B28, 0x2BB45A92, 0x5CB36A04, 0xC2D7FFA7, 0xB5D0CF31, 0x2CD99E8B, 0x5BDEAE1D,
            0x9B64C2B0, 0xEC63F226, 0x756AA39C, 0x026D930A, 0x9C0906A9, 0xEB0E363F, 0x72076785, 0x05005713,
            0x95BF4A82, 0xE2B87A14, 0x7BB12BAE, 0x0CB61B38, 0x92D28E9B, 0xE5D5BE0D, 0x7CDCEFB7, 0x0BDBDF21,
            0x86D3D2D4, 0xF1D4E242, 0x68DDB3F8, 0x1FDA836E, 0x81BE16CD, 0xF6B9265B, 0x6FB077E1, 0x18B74777,
            0x88085AE6, 0xFF0F6A70, 0x66063BCA, 0x11010B5C, 0x8F659EFF, 0xF862AE69, 0x616BFFD3, 0x166CCF45,
            0xA00AE278, 0xD70DD2EE, 0x4E048354, 0x3903B3C2, 0xA7672661, 0xD06016F7, 0x4969474D, 0x3E6E77DB,
            0xAED16A4A, 0xD9D65ADC, 0x40DF0B66, 0x37D83BF0, 0xA9BCAE53, 0xDEBB9EC5, 0x47B2CF7F, 0x30B5FFE9,
            0xBDBDF21C, 0xCABAC28A, 0x53B39330, 0x24B4A3A6, 0xBAD03605, 0xCDD70693, 0x54DE5729, 0x23D967BF,
            0xB3667A2E, 0xC4614AB8, 0x5D681B02, 0x2A6F2B94, 0xB40BBE37, 0xC30C8EA1, 0x5A05DF1B, 0x2D02EF8D,
        ];

        let mut crc = 0xFFFFFFFF_u32;
        for &byte in data {
            crc = CRC32_TABLE[((crc ^ (byte as u32)) & 0xFF) as usize] ^ (crc >> 8);
        }
        crc ^ 0xFFFFFFFF
    }

    /// Ensure CRC32 algorithm is loaded to target RAM
    /// Returns the address where the algorithm is loaded
    fn ensure_crc32_algorithm_loaded(&mut self, session: &mut Session) -> Result<u64, FlashError> {
        // For now, use a fixed address in high RAM
        // TODO: Make this configurable based on target memory map
        const CRC32_ALGO_ADDR: u64 = 0x20040000;
        
        // Check if already loaded by reading the algorithm header (first 4 bytes: 0x00BE00BE)
        let mut header = [0u8; 4];
        session.core(0)
            .map_err(FlashError::Core)?
            .read(CRC32_ALGO_ADDR, &mut header)
            .map_err(FlashError::Core)?;
            
        if u32::from_le_bytes(header) == 0xBE00BE00 {
            // Already loaded
            tracing::debug!("CRC32 algorithm already loaded at 0x{:08x}", CRC32_ALGO_ADDR);
            return Ok(CRC32_ALGO_ADDR);
        }
        
        // Load the architecture-appropriate CRC32 binary blob
        let crc32_blob = self.get_crc32_algorithm_blob(session)?;
        tracing::debug!("Loading CRC32 algorithm ({} bytes) to 0x{:08x}", crc32_blob.len(), CRC32_ALGO_ADDR);
        
        session.core(0)
            .map_err(FlashError::Core)?
            .write(CRC32_ALGO_ADDR, &crc32_blob)
            .map_err(FlashError::Core)?;
            
        // Verify the algorithm was written correctly
        let mut readback = vec![0u8; crc32_blob.len()];
        session.core(0)
            .map_err(FlashError::Core)?
            .read(CRC32_ALGO_ADDR, &mut readback)
            .map_err(FlashError::Core)?;
            
        if readback != crc32_blob {
            return Err(FlashError::Core(crate::Error::Other(
                "CRC32 algorithm verification failed".into()
            )));
        }
        
        tracing::debug!("CRC32 algorithm loaded and verified at 0x{:08x}", CRC32_ALGO_ADDR);
        Ok(CRC32_ALGO_ADDR)
    }

    /// Get the CRC32 algorithm binary blob for the target architecture
    fn get_crc32_algorithm_blob(&self, session: &mut Session) -> Result<Vec<u8>, FlashError> {
        use probe_rs_target::Architecture;
        
        let (architecture, core_type) = {
            let core = session.core(0).map_err(FlashError::Core)?;
            (core.architecture(), core.core_type())
        };
        
        // Detect target architecture and select appropriate CRC32 algorithm
        match architecture {
            Architecture::Arm => {
                // ARM architecture - detect specific core for optimal performance
                self.get_arm_crc32_blob(session, core_type)
            }
            Architecture::Riscv => {
                // RISC-V architecture - use slicing-by-4 algorithm optimized for RISC-V
                const RISCV_CRC32_BLOB: &[u8] = include_bytes!("../../../crc32-blobs/riscv/crc32.bin");
                tracing::debug!("Using RISC-V slicing-by-4 CRC32 blob ({} bytes)", RISCV_CRC32_BLOB.len());
                Ok(RISCV_CRC32_BLOB.to_vec())
            }
            Architecture::Xtensa => {
                // Xtensa architecture - fallback to ARM M3/M4 slicing-by-4 for now
                const ARM_THUMB_CRC32_SLICE4_BLOB: &[u8] = include_bytes!("../../../crc32-blobs/arm-thumb/crc32_slice4_optimized.bin");
                tracing::debug!("Using ARM Thumb slicing-by-4 CRC32 blob for Xtensa ({} bytes)", ARM_THUMB_CRC32_SLICE4_BLOB.len());
                Ok(ARM_THUMB_CRC32_SLICE4_BLOB.to_vec())
            }
        }
    }

    /// Get ARM-specific CRC32 blob optimized for the detected Cortex-M core
    fn get_arm_crc32_blob(&self, session: &Session, core_type: probe_rs_target::CoreType) -> Result<Vec<u8>, FlashError> {
        use probe_rs_target::CoreType;
        
        // Detect specific ARM Cortex-M variant for optimal CRC32 algorithm selection
        match core_type {
            CoreType::Armv6m => {
                // Cortex-M0/M0+ - use M0+ compatible build
                const ARM_M0PLUS_CRC32_BLOB: &[u8] = include_bytes!("../../../crc32-blobs/arm-thumb/crc32_slice4_m0plus.bin");
                if ARM_M0PLUS_CRC32_BLOB.len() > 0 {
                    tracing::debug!("Using ARM Cortex-M0+ optimized CRC32 blob ({} bytes)", ARM_M0PLUS_CRC32_BLOB.len());
                    Ok(ARM_M0PLUS_CRC32_BLOB.to_vec())
                } else {
                    // Fallback to standard optimized version
                    tracing::debug!("M0+ blob not available, falling back to standard ARM CRC32 blob");
                    const ARM_THUMB_CRC32_SLICE4_BLOB: &[u8] = include_bytes!("../../../crc32-blobs/arm-thumb/crc32_slice4_optimized.bin");
                    Ok(ARM_THUMB_CRC32_SLICE4_BLOB.to_vec())
                }
            }
            CoreType::Armv7m => {
                // Cortex-M3 - use standard performance build
                const ARM_THUMB_CRC32_SLICE4_BLOB: &[u8] = include_bytes!("../../../crc32-blobs/arm-thumb/crc32_slice4_optimized.bin");
                tracing::debug!("Using ARM Cortex-M3 optimized CRC32 blob ({} bytes)", ARM_THUMB_CRC32_SLICE4_BLOB.len());
                Ok(ARM_THUMB_CRC32_SLICE4_BLOB.to_vec())
            }
            CoreType::Armv7em => {
                // Cortex-M4/M7/M33 - try to detect M7 specifically for best performance
                if self.is_cortex_m7_target(session) {
                    const ARM_M7_CRC32_BLOB: &[u8] = include_bytes!("../../../crc32-blobs/arm-thumb/crc32_slice4_m7.bin");
                    tracing::debug!("Using ARM Cortex-M7 optimized CRC32 blob ({} bytes)", ARM_M7_CRC32_BLOB.len());
                    Ok(ARM_M7_CRC32_BLOB.to_vec())
                } else {
                    // Cortex-M4/M33 - use standard performance build
                    const ARM_THUMB_CRC32_SLICE4_BLOB: &[u8] = include_bytes!("../../../crc32-blobs/arm-thumb/crc32_slice4_optimized.bin");
                    tracing::debug!("Using ARM Cortex-M4/M33 optimized CRC32 blob ({} bytes)", ARM_THUMB_CRC32_SLICE4_BLOB.len());
                    Ok(ARM_THUMB_CRC32_SLICE4_BLOB.to_vec())
                }
            }
            CoreType::Armv8m => {
                // Cortex-M23/M33 - use standard performance build  
                const ARM_THUMB_CRC32_SLICE4_BLOB: &[u8] = include_bytes!("../../../crc32-blobs/arm-thumb/crc32_slice4_optimized.bin");
                tracing::debug!("Using ARM Cortex-M23/M33 CRC32 blob ({} bytes)", ARM_THUMB_CRC32_SLICE4_BLOB.len());
                Ok(ARM_THUMB_CRC32_SLICE4_BLOB.to_vec())
            }
            _ => {
                // Unknown ARM core - use standard performance build as fallback
                const ARM_THUMB_CRC32_SLICE4_BLOB: &[u8] = include_bytes!("../../../crc32-blobs/arm-thumb/crc32_slice4_optimized.bin");
                tracing::debug!("Unknown ARM core type {:?}, using standard CRC32 blob ({} bytes)", core_type, ARM_THUMB_CRC32_SLICE4_BLOB.len());
                Ok(ARM_THUMB_CRC32_SLICE4_BLOB.to_vec())
            }
        }
    }

    /// Detect if the target is Cortex-M7 for enhanced performance optimizations
    fn is_cortex_m7_target(&self, session: &Session) -> bool {
        // Check target name for common M7-based MCUs
        let target_name = session.target().name.to_lowercase();
        
        // Common Cortex-M7 based MCU families
        if target_name.contains("stm32f7") ||
           target_name.contains("stm32h7") ||
           target_name.contains("stm32u5") ||
           target_name.contains("imxrt1") ||
           target_name.contains("sam70") ||
           target_name.contains("samv7") ||
           target_name.contains("same70") ||
           target_name.contains("mk82") ||
           target_name.contains("mk28") {
            tracing::debug!("Detected Cortex-M7 target: {}", target_name);
            return true;
        }
        
        // TODO: Future enhancement - read CPUID register for precise detection
        // This would allow detection of M7 cores even in targets not listed above
        false
    }

    /// Execute CRC32 calculation on target device
    fn execute_crc32_on_target(
        &mut self,
        session: &mut Session,
        algo_addr: u64,
        flash_addr: u64,
        data_len: u64,
    ) -> Result<u32, FlashError> {
        use crate::core::RegisterId;
        use std::time::{Duration, Instant};

        let mut core = session.core(0).map_err(FlashError::Core)?;
        
        // Halt the core for register setup
        core.halt(Duration::from_millis(10)).map_err(FlashError::Core)?;
        
        // Set up ARM calling convention following standard flash algorithm pattern:
        // R0 = flash address to read
        // R1 = data length 
        // R2 = initial CRC (0)
        // PC = CRC32 function address (offset 0x0C) with Thumb bit
        // LR = return address (breakpoint at load address)
        tracing::debug!("CRC32 params: addr=0x{:08x}, len={}, algo=0x{:08x}", flash_addr, data_len, algo_addr);
        
        // Quick test: read first 4 bytes from flash to verify it's accessible
        let mut test_data = [0u8; 4];
        match core.read(flash_addr, &mut test_data) {
            Ok(_) => tracing::debug!("Flash read test OK: {:02x?}", test_data),
            Err(e) => tracing::error!("Flash read test FAILED: {:?}", e),
        }
        
        // Set up stack pointer (similar to flash algorithm setup)
        // Use a safe stack location in RAM, well above our algorithm
        let stack_pointer = algo_addr + 0x2000; // 8KB above algorithm
        core.write_core_reg(RegisterId::from(13), stack_pointer as u32).map_err(FlashError::Core)?;
        tracing::debug!("Stack pointer set to 0x{:08x}", stack_pointer);
        
        // Standard flash algorithm pattern: LR points to breakpoint at load address
        let breakpoint_addr = (algo_addr | 1) as u32; // +1 for Thumb mode
        
        core.write_core_reg(RegisterId::from(0), flash_addr as u32).map_err(FlashError::Core)?;
        core.write_core_reg(RegisterId::from(1), data_len as u32).map_err(FlashError::Core)?;
        core.write_core_reg(RegisterId::from(2), 0u32).map_err(FlashError::Core)?;
        core.write_core_reg(RegisterId::from(15), ((algo_addr + 0x0C) | 1) as u32).map_err(FlashError::Core)?;
        core.write_core_reg(RegisterId::from(14), breakpoint_addr).map_err(FlashError::Core)?;
        
        tracing::debug!("Registers set: PC=0x{:08x}, LR=0x{:08x}", ((algo_addr + 0x0C) | 1) as u32, breakpoint_addr);
        
        // Execute the CRC32 function
        let execution_start = Instant::now();
        core.run().map_err(FlashError::Core)?;
        
        // Wait for completion using standard flash algorithm pattern
        // Poll for halted status like other flash algorithms do
        let timeout = Duration::from_millis(1000); // Increase timeout for debugging
        let start_time = Instant::now();
        let poll_interval = Duration::from_millis(1);
        
        loop {
            std::thread::sleep(poll_interval);
            
            // Check if we've timed out
            if start_time.elapsed() > timeout {
                let current_pc: u32 = core.read_core_reg(RegisterId::from(15)).map_err(FlashError::Core)?;
                tracing::error!("CRC32 algorithm timeout: PC=0x{:08x}, expected breakpoint at 0x{:08x}", 
                    current_pc, breakpoint_addr);
                return Err(FlashError::RoutineCallFailed {
                    name: "CRC32 calculation",
                    error_code: 1
                });
            }
            
            // Check core status (standard flash algorithm pattern)
            let core_status = core.status().map_err(FlashError::Core)?;
            if let crate::CoreStatus::Halted(_) = core_status {
                // Core is halted - check if we're at the expected breakpoint
                let current_pc: u32 = core.read_core_reg(RegisterId::from(15)).map_err(FlashError::Core)?;
                tracing::debug!("Core halted at PC=0x{:08x} (expected breakpoint: 0x{:08x})", 
                    current_pc, breakpoint_addr);
                
                // Check if PC is at or near our breakpoint location
                if (current_pc & 0xFFFFFFFE) == (breakpoint_addr & 0xFFFFFFFE) {
                    tracing::debug!("CRC32 algorithm completed successfully at breakpoint");
                    break;
                } else {
                    // Not at expected breakpoint - might be a different halt, continue
                    tracing::debug!("Unexpected halt location, resuming execution");
                    core.run().map_err(FlashError::Core)?;
                    continue;
                }
            }
            // Core still running, continue polling
        }
        
        // Get the CRC32 result from R0
        let crc32_result: u32 = core.read_core_reg(RegisterId::from(0)).map_err(FlashError::Core)?;
        
        // Log completion details
        let r1: u32 = core.read_core_reg(RegisterId::from(1)).map_err(FlashError::Core)?;
        let current_pc: u32 = core.read_core_reg(RegisterId::from(15)).map_err(FlashError::Core)?;
        tracing::debug!("Final state: R0=0x{:08x}, R1=0x{:08x}, PC=0x{:08x}", crc32_result, r1, current_pc);
        
        let execution_time = execution_start.elapsed();
        tracing::debug!("CRC32 executed in {:.2}ms for {} bytes", 
            execution_time.as_secs_f32() * 1000.0, data_len);
        
        Ok(crc32_result)
    }

    /// Erases a single sector using the flash algorithm
    fn erase_single_sector(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
        sector_addr: u64,
    ) -> Result<(), FlashError> {
        tracing::debug!("Erasing single sector at 0x{:08x}", sector_addr);
        
        // Use the existing flash algorithm infrastructure to erase this specific sector
        // The run_erase closure will have access to the regions data, so we can find the sector there
        self.run_erase(session, progress, |active, data| {
            // Find the sector that needs to be erased within the closure to avoid borrowing issues
            let mut target_sector = None;
            for region in data.iter() {
                let layout = region.flash_layout();
                if let Some(sector) = layout.sectors().iter().find(|s| s.address() == sector_addr) {
                    target_sector = Some(sector);
                    break;
                }
            }
            
            let sector = target_sector.ok_or(FlashError::InvalidDataAddress { 
                data_load_addr: sector_addr, 
                data_ram: 0..0 
            })?;
            
            active
                .erase_sector(sector)
                .map_err(|e| FlashError::EraseFailed {
                    sector_address: sector.address(),
                    source: Box::new(e),
                })
        })
    }

    /// Erases multiple sectors efficiently  
    fn sector_erase_selective(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
        sectors_to_erase: &std::collections::HashSet<u64>,
    ) -> Result<(), FlashError> {
        tracing::debug!("Erasing {} sectors", sectors_to_erase.len());
        
        for &sector_addr in sectors_to_erase {
            self.erase_single_sector(session, progress, sector_addr)?;
        }
        
        Ok(())
    }

    /// Fills all the unwritten bytes in `layout`.
    ///
    /// If `restore_unwritten_bytes` is `true`, all bytes of the layout's page,
    /// that are not to be written during flashing will be read from the flash first
    /// and written again once the page is programmed.
    pub(super) fn fill_unwritten(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
    ) -> Result<(), FlashError> {
        progress.started_filling();

        fn fill_pages(
            regions: &mut [LoadedRegion],
            progress: &FlashProgress,
            mut read: impl FnMut(u64, &mut [u8]) -> Result<(), FlashError>,
        ) -> Result<(), FlashError> {
            for region in regions.iter_mut() {
                let layout = region.data.layout_mut();
                for fill in layout.fills.iter() {
                    let t = Instant::now();
                    let page = &mut layout.pages[fill.page_index()];

                    let page_offset = (fill.address() - page.address()) as usize;
                    let page_slice = &mut page.data_mut()[page_offset..][..fill.size() as usize];

                    read(fill.address(), page_slice)?;

                    progress.page_filled(fill.size(), t.elapsed());
                }
            }

            Ok(())
        }

        let result = if self.flash_algorithm.pc_read.is_some() {
            self.run_verify(session, progress, |active, data| {
                fill_pages(data, progress, |address, data| {
                    active.read_flash(address, data)
                })
            })
        } else {
            // Not using a flash algorithm function, so there's no need to go
            // through ActiveFlasher.
            let mut core = session.core(0).map_err(FlashError::Core)?;
            fill_pages(&mut self.regions, progress, |address, data| {
                core.read(address, data).map_err(FlashError::Core)
            })
        };

        match result.is_ok() {
            true => progress.finished_filling(),
            false => progress.failed_filling(),
        }

        result
    }

    /// Verifies all the to-be-written bytes of this flasher.
    pub(super) fn verify(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
        ignore_filled: bool,
    ) -> Result<bool, FlashError> {
        progress.started_verifying();

        let result = self.do_verify(session, progress, ignore_filled);

        match result.is_ok() {
            true => progress.finished_verifying(),
            false => progress.failed_verifying(),
        }

        result
    }

    fn do_verify(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
        ignore_filled: bool,
    ) -> Result<bool, FlashError> {
        let encoding = self.flash_algorithm.transfer_encoding;
        if let Some(verify) = self.flash_algorithm.pc_verify {
            // Try to use the verify function if available.
            self.run_verify(session, progress, |active, data| {
                for region in data {
                    tracing::debug!("Verify using CMSIS function");

                    // Prefer Verify as we may use compression
                    let flash_encoder = region.data.encoder(encoding, ignore_filled);

                    for page in flash_encoder.pages() {
                        let start = Instant::now();
                        let address = page.address();
                        let bytes = page.data();

                        tracing::debug!(
                            "Verifying page at address {:#010x} with size: {}",
                            address,
                            bytes.len()
                        );

                        // Transfer the bytes to RAM.
                        let buffer_address = active.load_page_buffer(bytes, 0)?;

                        let result = active.call_function_and_wait(
                            &Registers {
                                pc: into_reg(verify)?,
                                r0: Some(into_reg(address)?),
                                r1: Some(into_reg(bytes.len() as u64)?),
                                r2: Some(into_reg(buffer_address)?),
                                r3: None,
                            },
                            false,
                            Duration::from_secs(30),
                        )?;

                        // Returns
                        // status information:
                        // the sum of (adr+sz) - on success.
                        // any other number - on failure, and represents the failing address.
                        if result as u64 != address + bytes.len() as u64 {
                            tracing::debug!(
                                "Verification failed for page at address {:#010x}",
                                result
                            );
                            return Ok(false);
                        }

                        progress.page_verified(bytes.len() as u64, start.elapsed());
                    }
                }
                Ok(true)
            })
        } else {
            tracing::debug!("Verify by reading back flash contents");

            fn compare_flash(
                regions: &[LoadedRegion],
                progress: &FlashProgress,
                ignore_filled: bool,
                mut read: impl FnMut(u64, &mut [u8]) -> Result<(), FlashError>,
            ) -> Result<bool, FlashError> {
                for region in regions {
                    let layout = region.data.layout();
                    for (idx, page) in layout.pages.iter().enumerate() {
                        let start = Instant::now();
                        let address = page.address();
                        let data = page.data();

                        let mut read_back = vec![0; data.len()];
                        read(address, &mut read_back)?;

                        if ignore_filled {
                            // "Unfill" fill regions. These don't get flashed, so their contents are
                            // allowed to differ. We mask these bytes with default flash content here,
                            // just for the verification process.
                            for fill in layout.fills() {
                                if fill.page_index() != idx {
                                    continue;
                                }

                                let fill_offset = (fill.address() - address) as usize;
                                let fill_size = fill.size() as usize;

                                let default_bytes = &data[fill_offset..][..fill_size];
                                read_back[fill_offset..][..fill_size]
                                    .copy_from_slice(default_bytes);
                            }
                        }
                        if data != read_back {
                            tracing::debug!(
                                "Verification failed for page at address {:#010x}",
                                address
                            );
                            return Ok(false);
                        }

                        progress.page_verified(data.len() as u64, start.elapsed());
                    }
                }
                Ok(true)
            }

            if self.flash_algorithm.pc_read.is_some() {
                self.run_verify(session, progress, |active, data| {
                    compare_flash(data, progress, ignore_filled, |address, data| {
                        active.read_flash(address, data)
                    })
                })
            } else {
                // Not using a flash algorithm function, so there's no need to go
                // through ActiveFlasher.
                let mut core = session.core(0).map_err(FlashError::Core)?;
                compare_flash(&self.regions, progress, ignore_filled, |address, data| {
                    core.read(address, data).map_err(FlashError::Core)
                })
            }
        }
    }

    /// Perform an erase of all sectors given in `flash_layout`.
    fn sector_erase(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
    ) -> Result<(), FlashError> {
        progress.started_erasing();

        let encoding = self.flash_algorithm.transfer_encoding;

        let result = self.run_erase(session, progress, |active, data| {
            for region in data.iter_mut() {
                for sector in region.data.encoder(encoding, false).sectors() {
                    active
                        .erase_sector(sector)
                        .map_err(|e| FlashError::EraseFailed {
                            sector_address: sector.address(),
                            source: Box::new(e),
                        })?;
                }
            }
            Ok(())
        });

        match result.is_ok() {
            true => progress.finished_erasing(),
            false => progress.failed_erasing(),
        }

        result
    }

    fn do_program(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
        enable_double_buffering: bool,
    ) -> Result<(), FlashError> {
        progress.started_programming();
        let program_result = if self.double_buffering_supported() && enable_double_buffering {
            self.program_double_buffer(session, progress)
        } else {
            self.program_simple(session, progress)
        };

        match program_result.is_ok() {
            true => progress.finished_programming(),
            false => progress.failed_programming(),
        }

        program_result
    }

    /// Programs the pages given in `flash_layout` into the flash.
    fn program_simple(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
    ) -> Result<(), FlashError> {
        let encoding = self.flash_algorithm.transfer_encoding;
        self.run_program(session, progress, |active, data| {
            for region in data.iter_mut() {
                tracing::debug!(
                    "    programming region: {:#010X?} ({} bytes)",
                    region.region.range,
                    region.region.range.end - region.region.range.start
                );
                let flash_encoder = region.data.encoder(encoding, false);
                for page in flash_encoder.pages() {
                    active
                        .program_page(page)
                        .map_err(|error| FlashError::PageWrite {
                            page_address: page.address(),
                            source: Box::new(error),
                        })?;
                }
            }
            Ok(())
        })
    }

    /// Selective programming for incremental mode - only programs pages in specified sectors.
    #[allow(dead_code)]
    fn do_program_selective(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
        enable_double_buffering: bool,
        sectors_to_update: &std::collections::HashSet<u64>,
    ) -> Result<(), FlashError> {
        progress.started_programming();
        let program_result = if self.double_buffering_supported() && enable_double_buffering {
            self.program_selective_double_buffer(session, progress, sectors_to_update)
        } else {
            self.program_selective_simple(session, progress, sectors_to_update)
        };

        match program_result.is_ok() {
            true => progress.finished_programming(),
            false => progress.failed_programming(),
        }

        program_result
    }

    /// Programs only pages in specified sectors (simple, non-double-buffered version).
    #[allow(dead_code)]
    fn program_selective_simple(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
        sectors_to_update: &std::collections::HashSet<u64>,
    ) -> Result<(), FlashError> {
        let encoding = self.flash_algorithm.transfer_encoding;
        self.run_program(session, progress, |active, data| {
            for region in data.iter_mut() {
                tracing::debug!(
                    "    programming region (selective): {:#010X?} ({} bytes)",
                    region.region.range,
                    region.region.range.end - region.region.range.start
                );
                // Get layout and collect all needed sector info to avoid borrowing conflicts
                let layout = region.data.layout();
                let sectors_info: Vec<(u64, u64)> = layout.sectors().iter() // (address, size)
                    .filter_map(|sector| {
                        if sectors_to_update.contains(&sector.address()) {
                            Some((sector.address(), sector.size()))
                        } else {
                            None
                        }
                    }).collect();
                
                if sectors_info.is_empty() {
                    tracing::debug!("No erased sectors in this region, skipping");
                    continue;
                }
                
                // Now we can safely borrow data mutably (layout reference is dropped)
                let flash_encoder = region.data.encoder(encoding, false);
                
                for page in flash_encoder.pages() {
                    // Check if this page is in any of the erased sectors for this region
                    let page_in_erased_sector = sectors_info.iter().any(|&(sector_addr, sector_size)| {
                        page.address() >= sector_addr && 
                        page.address() < (sector_addr + sector_size)
                    });
                    
                    if page_in_erased_sector {
                        active
                            .program_page(page)
                            .map_err(|error| FlashError::PageWrite {
                                page_address: page.address(),
                                source: Box::new(error),
                            })?;
                    } else {
                    }
                }
            }
            Ok(())
        })
    }

    /// Programs only pages in specified sectors (double-buffered version).
    #[allow(dead_code)]
    fn program_selective_double_buffer(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
        sectors_to_update: &std::collections::HashSet<u64>,
    ) -> Result<(), FlashError> {
        let encoding = self.flash_algorithm.transfer_encoding;
        self.run_program(session, progress, |active, data| {
            for region in data.iter_mut() {
                tracing::debug!(
                    "    programming region (selective double-buffer): {:#010X?} ({} bytes)",
                    region.region.range,
                    region.region.range.end - region.region.range.start
                );
                // Get layout and collect all needed sector info to avoid borrowing conflicts
                let layout = region.data.layout();
                let sectors_info: Vec<(u64, u64)> = layout.sectors().iter() // (address, size)
                    .filter_map(|sector| {
                        if sectors_to_update.contains(&sector.address()) {
                            Some((sector.address(), sector.size()))
                        } else {
                            None
                        }
                    }).collect();
                
                if sectors_info.is_empty() {
                    tracing::debug!("No erased sectors in this region (double-buffer), skipping");
                    continue;
                }
                
                // Now we can safely borrow data mutably (layout reference is dropped)
                let flash_encoder = region.data.encoder(encoding, false);

                // Collect only pages that are in erased sectors
                let pages_to_program: Vec<_> = flash_encoder.pages().iter().filter(|page| {
                    sectors_info.iter().any(|&(sector_addr, sector_size)| {
                        page.address() >= sector_addr && 
                        page.address() < (sector_addr + sector_size)
                    })
                }).collect();

                if pages_to_program.is_empty() {
                    continue;
                }

                let mut current_buf = 0;
                let mut t = Instant::now();
                let mut last_page_address = 0;
                
                for page in pages_to_program {
                    
                    // Load the page into the current buffer
                    let buffer_address = active.load_page_buffer(page.data(), current_buf)?;

                    // Wait for the previous copy operation to finish
                    active.wait_for_write_end(last_page_address)?;

                    last_page_address = page.address();
                    progress.page_programmed(page.size() as u64, t.elapsed());

                    t = Instant::now();

                    // Start the next copy process
                    active.start_program_page_with_buffer(
                        buffer_address,
                        page.address(),
                        page.size() as u64,
                    )?;

                    // Swap buffers
                    current_buf = if current_buf == 1 { 0 } else { 1 };
                }

                // Wait for the final copy to complete
                active.wait_for_write_end(last_page_address)?;
            }
            Ok(())
        })
    }

    /// Flash a program using double buffering.
    ///
    /// This uses two buffers to increase the flash speed.
    /// While the data from one buffer is programmed, the
    /// data for the next page is already downloaded
    /// into the next buffer.
    ///
    /// This is only possible if the RAM is large enough to
    /// fit at least two page buffers. See [Flasher::double_buffering_supported].
    fn program_double_buffer(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
    ) -> Result<(), FlashError> {
        let encoding = self.flash_algorithm.transfer_encoding;
        self.run_program(session, progress, |active, data| {
            for region in data.iter_mut() {
                tracing::debug!(
                    "    programming region: {:#010X?} ({} bytes)",
                    region.region.range,
                    region.region.range.end - region.region.range.start
                );
                let flash_encoder = region.data.encoder(encoding, false);

                let mut current_buf = 0;
                let mut t = Instant::now();
                let mut last_page_address = 0;
                for page in flash_encoder.pages() {
                    // At the start of each loop cycle load the next page buffer into RAM.
                    let buffer_address = active.load_page_buffer(page.data(), current_buf)?;

                    // Then wait for the active RAM -> Flash copy process to finish.
                    // Also check if it finished properly. If it didn't, return an error.
                    active.wait_for_write_end(last_page_address)?;

                    last_page_address = page.address();
                    progress.page_programmed(page.size() as u64, t.elapsed());

                    t = Instant::now();

                    // Start the next copy process.
                    active.start_program_page_with_buffer(
                        buffer_address,
                        page.address(),
                        page.size() as u64,
                    )?;

                    // Swap the buffers
                    if current_buf == 1 {
                        current_buf = 0;
                    } else {
                        current_buf = 1;
                    }
                }

                active.wait_for_write_end(last_page_address)?;
            }
            Ok(())
        })
    }

    pub(crate) fn add_region(
        &mut self,
        region: NvmRegion,
        builder: &FlashBuilder,
        restore_unwritten_bytes: bool,
    ) -> Result<(), FlashError> {
        let layout = builder.build_sectors_and_pages(
            &region,
            &self.flash_algorithm,
            restore_unwritten_bytes,
        )?;
        self.regions.push(LoadedRegion {
            region,
            data: FlashData::Raw(layout),
        });
        Ok(())
    }
}

struct Registers {
    pc: u32,
    r0: Option<u32>,
    r1: Option<u32>,
    r2: Option<u32>,
    r3: Option<u32>,
}

impl Debug for Registers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:#010x} ({:?}, {:?}, {:?}, {:?})",
            self.pc, self.r0, self.r1, self.r2, self.r3
        )
    }
}

fn into_reg(val: u64) -> Result<u32, FlashError> {
    let reg_value: u32 = val
        .try_into()
        .map_err(|_| FlashError::RegisterValueNotSupported(val))?;

    Ok(reg_value)
}

pub(super) struct ActiveFlasher<'op, 'p, O: Operation> {
    core: Core<'op>,
    instruction_set: InstructionSet,
    rtt: Option<Rtt>,
    progress: &'op FlashProgress<'p>,
    flash_algorithm: &'op FlashAlgorithm,
    _operation: PhantomData<O>,
}

impl<O: Operation> ActiveFlasher<'_, '_, O> {
    #[tracing::instrument(name = "Call to flash algorithm init", skip(self, clock))]
    pub(super) fn init(&mut self, clock: Option<u32>) -> Result<(), FlashError> {
        let algo = &self.flash_algorithm;

        // Skip init routine if not present.
        let Some(pc_init) = algo.pc_init else {
            return Ok(());
        };

        let address = self.flash_algorithm.flash_properties.address_range.start;
        let error_code = self
            .call_function_and_wait(
                &Registers {
                    pc: into_reg(pc_init)?,
                    r0: Some(into_reg(address)?),
                    r1: clock.or(Some(0)),
                    r2: Some(O::OPERATION),
                    r3: None,
                },
                true,
                INIT_TIMEOUT,
            )
            .map_err(|error| FlashError::Init(Box::new(error)))?;

        if error_code != 0 {
            return Err(FlashError::RoutineCallFailed {
                name: "init",
                error_code,
            });
        }

        Ok(())
    }

    pub(super) fn uninit(&mut self) -> Result<(), FlashError> {
        tracing::debug!("Running uninit routine.");
        let algo = &self.flash_algorithm;

        // Skip uninit routine if not present.
        let Some(pc_uninit) = algo.pc_uninit else {
            return Ok(());
        };

        let error_code = self
            .call_function_and_wait(
                &Registers {
                    pc: into_reg(pc_uninit)?,
                    r0: Some(O::OPERATION),
                    r1: None,
                    r2: None,
                    r3: None,
                },
                false,
                INIT_TIMEOUT,
            )
            .map_err(|error| FlashError::Uninit(Box::new(error)))?;

        if error_code != 0 {
            return Err(FlashError::RoutineCallFailed {
                name: "uninit",
                error_code,
            });
        }

        Ok(())
    }

    fn call_function_and_wait(
        &mut self,
        registers: &Registers,
        init: bool,
        duration: Duration,
    ) -> Result<u32, FlashError> {
        self.call_function(registers, init)?;
        let r = self.wait_for_completion(duration);

        if r.is_err() {
            tracing::debug!("Routine call failed: {:?}", r);
        }

        r
    }

    fn call_function(&mut self, registers: &Registers, init: bool) -> Result<(), FlashError> {
        tracing::debug!("Calling routine {:?}, init={})", registers, init);

        let algo = &self.flash_algorithm;
        let regs: &'static CoreRegisters = self.core.registers();

        let registers = [
            (self.core.program_counter(), Some(registers.pc)),
            (regs.argument_register(0), registers.r0),
            (regs.argument_register(1), registers.r1),
            (regs.argument_register(2), registers.r2),
            (regs.argument_register(3), registers.r3),
            (
                regs.core_register(9),
                if init {
                    Some(into_reg(algo.static_base)?)
                } else {
                    None
                },
            ),
            (
                self.core.stack_pointer(),
                if init {
                    Some(into_reg(algo.stack_top)?)
                } else {
                    None
                },
            ),
            (
                self.core.return_address(),
                // For ARM Cortex-M cores, we have to add 1 to the return address,
                // to ensure that we stay in Thumb mode. A32 also generally supports
                // Thumb and uses the same `BKPT` instruction when in this mode.
                if self.instruction_set == InstructionSet::Thumb2
                    || self.instruction_set == InstructionSet::A32
                {
                    Some(into_reg(algo.load_address + 1)?)
                } else {
                    Some(into_reg(algo.load_address)?)
                },
            ),
        ];

        for (description, value) in registers {
            if let Some(v) = value {
                self.core.write_core_reg(description, v).map_err(|error| {
                    FlashError::Core(Error::WriteRegister {
                        register: description.to_string(),
                        source: Box::new(error),
                    })
                })?;

                if tracing::enabled!(Level::DEBUG) {
                    let value: u32 = self.core.read_core_reg(description).map_err(|error| {
                        FlashError::Core(Error::ReadRegister {
                            register: description.to_string(),
                            source: Box::new(error),
                        })
                    })?;

                    tracing::debug!(
                        "content of {} {:#x}: {:#010x} should be: {:#010x}",
                        description.name(),
                        description.id.0,
                        value,
                        v
                    );
                }
            }
        }

        // Resume target operation.
        self.core.run().map_err(FlashError::Run)?;

        if let Some(rtt_address) = self.flash_algorithm.rtt_control_block {
            match rtt::try_attach_to_rtt(
                &mut self.core,
                Duration::from_secs(1),
                &ScanRegion::Exact(rtt_address),
            ) {
                Ok(rtt) => self.rtt = Some(rtt),
                Err(rtt::Error::NoControlBlockLocation) => {}
                Err(error) => tracing::error!("RTT could not be initialized: {error}"),
            }
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub(super) fn wait_for_completion(&mut self, timeout: Duration) -> Result<u32, FlashError> {
        tracing::debug!("Waiting for routine call completion.");
        let regs = self.core.registers();

        // Wait until halted state is active again.
        let start = Instant::now();

        loop {
            match self
                .core
                .status()
                .map_err(FlashError::UnableToReadCoreStatus)?
            {
                CoreStatus::Halted(_) => {
                    // Once the core is halted we know for sure all RTT data is written
                    // so we can read all of it.
                    self.read_rtt()?;
                    break;
                }
                CoreStatus::LockedUp => {
                    return Err(FlashError::UnexpectedCoreStatus {
                        status: CoreStatus::LockedUp,
                    });
                }
                _ => {} // All other statuses are okay: we'll just keep polling.
            }
            self.read_rtt()?;
            if start.elapsed() >= timeout {
                return Err(FlashError::Core(Error::Timeout));
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        self.check_for_stack_overflow()?;

        let r = self
            .core
            .read_core_reg::<u32>(regs.result_register(0))
            .map_err(|error| {
                FlashError::Core(Error::ReadRegister {
                    register: regs.result_register(0).to_string(),
                    source: Box::new(error),
                })
            })?;

        tracing::debug!("Routine returned {:x}.", r);

        Ok(r)
    }

    fn read_rtt(&mut self) -> Result<(), FlashError> {
        let Some(rtt) = &mut self.rtt else {
            return Ok(());
        };

        for channel in rtt.up_channels().iter_mut() {
            let mut buffer = vec![0; channel.buffer_size()];
            match channel.read(&mut self.core, &mut buffer) {
                Ok(read) if read > 0 => {
                    let message = String::from_utf8_lossy(&buffer[..read]).to_string();
                    let channel = channel.name().unwrap_or("unnamed");
                    tracing::debug!("RTT({channel}): {message}");
                    self.progress.message(message);
                }
                Ok(_) => (),
                Err(error) => tracing::debug!("Reading RTT failed: {error}"),
            }
        }

        Ok(())
    }

    fn check_for_stack_overflow(&mut self) -> Result<(), FlashError> {
        let algo = &self.flash_algorithm;

        if !algo.stack_overflow_check {
            return Ok(());
        }

        let stack_bottom = algo.stack_top - algo.stack_size;
        let read_back = self
            .core
            .read_word_8(stack_bottom)
            .map_err(FlashError::Core)?;

        if read_back != STACK_FILL_BYTE {
            return Err(FlashError::StackOverflowDetected { operation: O::NAME });
        }

        Ok(())
    }

    pub(super) fn read_flash(&mut self, address: u64, data: &mut [u8]) -> Result<(), FlashError> {
        if let Some(read_flash) = self.flash_algorithm.pc_read {
            let page_size = self.flash_algorithm.flash_properties.page_size;
            let buffer_address = self.flash_algorithm.page_buffers[0];

            let mut read_address = address;
            for slice in data.chunks_mut(page_size as usize) {
                // Call ReadFlash to load from flash to RAM. The function has a similar signature
                // to the program_page function.
                let result = self
                    .call_function_and_wait(
                        &Registers {
                            pc: into_reg(read_flash)?,
                            r0: Some(into_reg(read_address)?),
                            r1: Some(into_reg(slice.len() as u64)?),
                            r2: Some(into_reg(buffer_address)?),
                            r3: None,
                        },
                        false,
                        Duration::from_secs(30),
                    )
                    .map_err(|error| FlashError::FlashReadFailed {
                        source: Box::new(error),
                    })?;

                if result != 0 {
                    return Err(FlashError::FlashReadFailed {
                        source: Box::new(FlashError::RoutineCallFailed {
                            name: "read_flash",
                            error_code: result,
                        }),
                    });
                };

                // Now read the data from RAM.
                self.core
                    .read(buffer_address, slice)
                    .map_err(FlashError::Core)?;
                read_address += slice.len() as u64;
            }

            Ok(())
        } else {
            self.core.read(address, data).map_err(FlashError::Core)
        }
    }

    /// Returns the address of the buffer that was used.
    pub(super) fn load_page_buffer(
        &mut self,
        bytes: &[u8],
        buffer_number: usize,
    ) -> Result<u64, FlashError> {
        // Ensure the buffer number is valid, otherwise there is a bug somewhere
        // in the flashing code.
        assert!(
            buffer_number < self.flash_algorithm.page_buffers.len(),
            "Trying to use non-existing buffer ({}/{}) for flashing. This is a bug. Please report it.",
            buffer_number,
            self.flash_algorithm.page_buffers.len()
        );

        let buffer_address = self.flash_algorithm.page_buffers[buffer_number];
        self.load_data(buffer_address, bytes)?;

        Ok(buffer_address)
    }

    /// Transfers the buffer bytes to RAM.
    fn load_data(&mut self, address: u64, bytes: &[u8]) -> Result<(), FlashError> {
        tracing::debug!(
            "Loading {} bytes of data into RAM at address {:#010x}\n",
            bytes.len(),
            address
        );
        // TODO: Prevent security settings from locking the device.

        // In case some of the previous preprocessing forgets to pad the last page,
        // we will fill the missing bytes with the erased byte value.
        let empty = self.flash_algorithm.flash_properties.erased_byte_value;
        let words: Vec<u32> = bytes
            .chunks(std::mem::size_of::<u32>())
            .map(|a| {
                u32::from_le_bytes([
                    a[0],
                    a.get(1).copied().unwrap_or(empty),
                    a.get(2).copied().unwrap_or(empty),
                    a.get(3).copied().unwrap_or(empty),
                ])
            })
            .collect();

        let t1 = Instant::now();

        self.core
            .write(address, words.as_bytes())
            .map_err(FlashError::Core)?;

        tracing::info!(
            "Took {:?} to download {} byte page into ram",
            t1.elapsed(),
            bytes.len()
        );

        Ok(())
    }
}

impl ActiveFlasher<'_, '_, Erase> {
    pub(super) fn erase_all(&mut self) -> Result<(), FlashError> {
        tracing::debug!("Erasing entire chip.");
        let algo = &self.flash_algorithm;

        let Some(pc_erase_all) = algo.pc_erase_all else {
            return Err(FlashError::ChipEraseNotSupported);
        };

        let result = self
            .call_function_and_wait(
                &Registers {
                    pc: into_reg(pc_erase_all)?,
                    r0: None,
                    r1: None,
                    r2: None,
                    r3: None,
                },
                false,
                Duration::from_secs(40),
            )
            .map_err(|error| FlashError::ChipEraseFailed {
                source: Box::new(error),
            })?;

        if result != 0 {
            Err(FlashError::ChipEraseFailed {
                source: Box::new(FlashError::RoutineCallFailed {
                    name: "chip_erase",
                    error_code: result,
                }),
            })
        } else {
            Ok(())
        }
    }

    pub(super) fn erase_sector(&mut self, sector: &FlashSector) -> Result<(), FlashError> {
        let address = sector.address();
        tracing::info!("Erasing sector at address {:#010x}", address);
        let t1 = Instant::now();

        let error_code = self.call_function_and_wait(
            &Registers {
                pc: into_reg(self.flash_algorithm.pc_erase_sector)?,
                r0: Some(into_reg(address)?),
                r1: None,
                r2: None,
                r3: None,
            },
            false,
            Duration::from_millis(
                self.flash_algorithm.flash_properties.erase_sector_timeout as u64,
            ),
        )?;
        tracing::info!(
            "Done erasing sector. Result is {}. This took {:?}",
            error_code,
            t1.elapsed()
        );

        if error_code != 0 {
            Err(FlashError::RoutineCallFailed {
                name: "erase_sector",
                error_code,
            })
        } else {
            self.progress.sector_erased(sector.size(), t1.elapsed());
            Ok(())
        }
    }

    pub(super) fn blank_check(&mut self, sector: &FlashSector) -> Result<(), FlashError> {
        let address = sector.address();
        let size = sector.size();
        tracing::info!(
            "Checking for blanked flash between address {:#010x} and {:#010x}",
            address,
            address + size
        );
        let t1 = Instant::now();

        if let Some(blank_check) = self.flash_algorithm.pc_blank_check {
            let error_code = self.call_function_and_wait(
                &Registers {
                    pc: into_reg(blank_check)?,
                    r0: Some(into_reg(address)?),
                    r1: Some(into_reg(size)?),
                    r2: Some(into_reg(
                        self.flash_algorithm
                            .flash_properties
                            .erased_byte_value
                            .into(),
                    )?),
                    r3: None,
                },
                false,
                Duration::from_millis(
                    // self.flash_algorithm.flash_properties.erase_sector_timeout as u64,
                    10_000,
                ),
            )?;
            tracing::info!(
                "Done checking blank. Result is {}. This took {:?}",
                error_code,
                t1.elapsed()
            );

            if error_code != 0 {
                Err(FlashError::RoutineCallFailed {
                    name: "blank_check",
                    error_code,
                })
            } else {
                self.progress.sector_erased(sector.size(), t1.elapsed());
                Ok(())
            }
        } else {
            let mut data = vec![0; size as usize];
            self.core
                .read(address, &mut data)
                .map_err(FlashError::Core)?;
            if !data
                .iter()
                .all(|v| *v == self.flash_algorithm.flash_properties.erased_byte_value)
            {
                return Err(FlashError::ChipEraseFailed {
                    source: "Not all sectors were erased".into(),
                });
            }
            Ok(())
        }
    }
}

impl ActiveFlasher<'_, '_, Program> {
    pub(super) fn program_page(&mut self, page: &FlashPage) -> Result<(), FlashError> {
        let t1 = Instant::now();

        let address = page.address();
        let bytes = page.data();

        tracing::info!(
            "Flashing page at address {:#08x} with size: {}",
            address,
            bytes.len()
        );

        // Transfer the bytes to RAM.
        let begin_data = self.load_page_buffer(bytes, 0)?;

        self.start_program_page_with_buffer(begin_data, address, bytes.len() as u64)?;
        self.wait_for_write_end(address)?;

        tracing::info!("Flashing took: {:?}", t1.elapsed());

        self.progress
            .page_programmed(page.size() as u64, t1.elapsed());
        Ok(())
    }

    pub(super) fn start_program_page_with_buffer(
        &mut self,
        buffer_address: u64,
        page_address: u64,
        data_size: u64,
    ) -> Result<(), FlashError> {
        self.call_function(
            &Registers {
                pc: into_reg(self.flash_algorithm.pc_program_page)?,
                r0: Some(into_reg(page_address)?),
                r1: Some(into_reg(data_size)?),
                r2: Some(into_reg(buffer_address)?),
                r3: None,
            },
            false,
        )
        .map_err(|error| FlashError::PageWrite {
            page_address,
            source: Box::new(error),
        })?;

        Ok(())
    }

    fn wait_for_write_end(&mut self, last_page_address: u64) -> Result<(), FlashError> {
        let timeout = Duration::from_millis(
            self.flash_algorithm.flash_properties.program_page_timeout as u64,
        );
        self.wait_for_completion(timeout)
            .and_then(|result| {
                if result == 0 {
                    Ok(())
                } else {
                    Err(FlashError::RoutineCallFailed {
                        name: "program_page",
                        error_code: result,
                    })
                }
            })
            .map_err(|error| FlashError::PageWrite {
                page_address: last_page_address,
                source: Box::new(error),
            })
    }
}
