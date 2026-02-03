use probe_rs_target::{RawFlashAlgorithm, TransferEncoding};
use tracing::Level;
use zerocopy::IntoBytes;

use super::{FlashAlgorithm, FlashBuilder, FlashError, FlashPage, FlashProgress};
use crate::config::NvmRegion;
use crate::error::Error;
use crate::flashing::encoder::FlashEncoder;
use crate::flashing::{FlashLayout, FlashSector};
use crate::memory::MemoryInterface;
use crate::rtt::{Rtt, ScanRegion};
use crate::{Core, InstructionSet, core::CoreRegisters, session::Session};
use crate::{CoreStatus, Target};
use std::borrow::Cow;
use std::marker::PhantomData;
use std::{
    fmt::Debug,
    time::{Duration, Instant},
};

/// The timeout for init/uninit routines.
const INIT_TIMEOUT: Duration = Duration::from_secs(2);

/// Represents the operation for which the flash loader is initialized.
pub trait Operation {
    const OPERATION: u32;
    const NAME: &'static str;
}

/// Type state for [`ActiveFlasher`] when the flash loader is initialized for erasing flash.
pub struct Erase;

impl Operation for Erase {
    const OPERATION: u32 = 1;
    const NAME: &'static str = "Erase";
}

/// Type state for [`ActiveFlasher`] when the flash loader is initialized for programming.
pub struct Program;

impl Operation for Program {
    const OPERATION: u32 = 2;
    const NAME: &'static str = "Program";
}

/// Type state for [`ActiveFlasher`] when the flash loader is initialized for verification.
pub struct Verify;

impl Operation for Verify {
    const OPERATION: u32 = 3;
    const NAME: &'static str = "Verify";
}

/// Flash data
// TODO this is hard to document because this seems like a bad API.
pub enum FlashData {
    /// Raw flash data.
    Raw(FlashLayout),

    /// Encoded flash data.
    Loaded {
        /// The flash encoder.
        encoder: FlashEncoder,

        /// Whether the encoder should ignore fill bytes during processing.
        ignore_fills: bool,
    },
}

impl FlashData {
    /// Returns a reference to the flash layout.
    pub fn layout(&self) -> &FlashLayout {
        match self {
            FlashData::Raw(layout) => layout,
            FlashData::Loaded { encoder, .. } => encoder.flash_layout(),
        }
    }

    /// Returns a reference to the flash layout.
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

    /// Returns the encoded data.
    pub fn encoder(&mut self, encoding: TransferEncoding, ignore_fills: bool) -> &FlashEncoder {
        if let FlashData::Loaded {
            encoder,
            ignore_fills: was_ignore_fills,
        } = self
            && *was_ignore_fills != ignore_fills
        {
            // Fill handling changed, invalidate the encoder
            *self = FlashData::Raw(encoder.flash_layout().clone());
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

/// Represents a piece of data to be flashed.
pub struct LoadedRegion {
    /// The region to flash data to.
    pub region: NvmRegion,

    /// The flash data of the loaded region.
    pub data: FlashData,
}

impl LoadedRegion {
    /// Returns the flash layout of the loaded region.
    pub fn flash_layout(&self) -> &FlashLayout {
        self.data.layout()
    }
}

/// A structure to control the flash of an attached microchip.
///
/// Once constructed it can be used to program date to the flash.
pub struct Flasher {
    pub(super) core_index: usize,
    pub(super) flash_algorithm: FlashAlgorithm,
    pub(super) loaded: bool,
    pub(super) regions: Vec<LoadedRegion>,
    pub(super) read_flasher_rtt: bool,
}

/// The byte used to fill the stack when checking for stack overflows.
const STACK_FILL_BYTE: u8 = 0x56;

impl Flasher {
    /// Creates a new Flasher object.
    pub fn new(
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
            read_flasher_rtt: false,
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

    /// Prepares the flashing algorithm.
    ///
    /// This function ensures that the flashing algorithm has been loaded into memory, and
    /// initialized for the given [`Operation].
    ///
    /// The `clk` argument specifies the clock frequency for prgramming the device.
    pub fn init<'s, 'p, O: Operation>(
        &'s mut self,
        session: &'s mut Session,
        progress: &'s mut FlashProgress<'p>,
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
            read_flasher_rtt: self.read_flasher_rtt,
            _operation: PhantomData,
        };

        flasher.init(clock)?;

        Ok((flasher, &mut self.regions))
    }

    /// Erases all flash memory using a debug sequence.
    pub fn run_erase_all(
        &mut self,
        session: &mut Session,
        progress: &mut FlashProgress<'_>,
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

    /// Initializes the flashing algorithm for the [`Erase`] operation and provides an interface to it via the callback.
    pub fn run_erase<'p, T, F>(
        &mut self,
        session: &mut Session,
        progress: &mut FlashProgress<'p>,
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

    /// Initializes the flashing algorithm for the [`Program`] operation and provides an interface to it via the callback.
    pub fn run_program<'p, T, F>(
        &mut self,
        session: &mut Session,
        progress: &mut FlashProgress<'p>,
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

    /// Initializes the flashing algorithm for the [`Verify`] operation and provides an interface to it via the callback.
    pub fn run_verify<'p, T, F>(
        &mut self,
        session: &mut Session,
        progress: &mut FlashProgress<'p>,
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
        progress: &mut FlashProgress<'_>,
        restore_unwritten_bytes: bool,
        enable_double_buffering: bool,
        skip_erasing: bool,
        verify: bool,
    ) -> Result<(), FlashError> {
        tracing::debug!("Starting program procedure.");

        tracing::debug!("Double Buffering enabled: {:?}", enable_double_buffering);
        tracing::debug!(
            "Restoring unwritten bytes enabled: {:?}",
            restore_unwritten_bytes
        );

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

    /// Fills all the unwritten bytes in `layout`.
    ///
    /// If `restore_unwritten_bytes` is `true`, all bytes of the layout's page,
    /// that are not to be written during flashing will be read from the flash first
    /// and written again once the page is programmed.
    pub(super) fn fill_unwritten(
        &mut self,
        session: &mut Session,
        progress: &mut FlashProgress<'_>,
    ) -> Result<(), FlashError> {
        progress.started_filling();

        fn fill_pages(
            regions: &mut [LoadedRegion],
            progress: &mut FlashProgress<'_>,
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
            self.run_verify(session, &mut FlashProgress::empty(), |active, data| {
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
        progress: &mut FlashProgress<'_>,
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
        progress: &mut FlashProgress<'_>,
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

                        active
                            .progress
                            .page_verified(bytes.len() as u64, start.elapsed());
                    }
                }
                Ok(true)
            })
        } else {
            tracing::debug!("Verify by reading back flash contents");

            fn compare_flash(
                regions: &[LoadedRegion],
                progress: &mut FlashProgress<'_>,
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
                self.run_verify(session, &mut FlashProgress::empty(), |active, data| {
                    compare_flash(data, progress, ignore_filled, |address, data| {
                        active.read_flash(address, data)
                    })
                })
            } else {
                // Not using a flash algorithm function, so there's no need to go
                // through ActiveFlasher.
                let mut core = session.core(self.core_index).map_err(FlashError::Core)?;
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
        progress: &mut FlashProgress<'_>,
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
        progress: &mut FlashProgress<'_>,
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
        progress: &mut FlashProgress<'_>,
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
        progress: &mut FlashProgress<'_>,
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
                    active
                        .progress
                        .page_programmed(page.size() as u64, t.elapsed());

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

    pub(crate) fn read_rtt_output(&mut self, read: bool) {
        self.read_flasher_rtt = read;
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

/// An initialized flash algorithm function interface.
pub struct ActiveFlasher<'op, 'p, O: Operation> {
    pub(super) core: Core<'op>,
    instruction_set: InstructionSet,
    rtt: Option<Rtt>,
    progress: &'op mut FlashProgress<'p>,
    flash_algorithm: &'op FlashAlgorithm,
    read_flasher_rtt: bool,
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

    /// Runs the ESP32-specific `read_flash_size` routine.
    // TODO: this function should not be defined in the probe-rs library.
    // The target yaml should provide a way to define custom functions, and ActiveFlasher
    // should provide a way to call any custom function without apriori knowledge.
    pub fn read_flash_size(&mut self) -> Result<u32, FlashError> {
        tracing::debug!("Reading flash size.");
        let algo = &self.flash_algorithm;

        // Fail routine if not present.
        let Some(pc_flash_size) = algo.pc_flash_size else {
            return Err(FlashError::FlashSizeFailed {
                source: String::from("Flash algorithm does not implement the FlashSize function")
                    .into(),
            });
        };

        let retval = self
            .call_function_and_wait(
                &Registers {
                    pc: into_reg(pc_flash_size)?,
                    r0: None,
                    r1: None,
                    r2: None,
                    r3: None,
                },
                false,
                INIT_TIMEOUT,
            )
            .map_err(|error| FlashError::FlashSizeFailed {
                source: Box::new(error),
            })?;

        if (retval as i32) < 0 {
            return Err(FlashError::RoutineCallFailed {
                name: "flash_size",
                error_code: retval,
            });
        }

        Ok(retval)
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
                // to ensure that we stay in Thumb mode.
                if self.instruction_set == InstructionSet::Thumb2 {
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

        if let Some(rtt_address) = self.flash_algorithm.rtt_control_block
            && self.rtt.is_none()
            && self.read_flasher_rtt
        {
            match crate::rtt::try_attach_to_rtt(
                &mut self.core,
                Duration::from_secs(1),
                &ScanRegion::Exact(rtt_address),
            ) {
                Ok(rtt) => self.rtt = Some(rtt),
                Err(crate::rtt::Error::NoControlBlockLocation) => {}
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
        let mut last_read = Instant::now();

        let poll_interval = Duration::from_millis(self.flash_algorithm.rtt_poll_interval);

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

            let now = Instant::now();

            if now - last_read >= poll_interval {
                self.read_rtt()?;
                last_read = now;
            }
            if now - start >= timeout {
                self.read_rtt()?;
                return Err(FlashError::Core(Error::Timeout));
            }
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

        let t1 = if tracing::enabled!(Level::INFO) {
            Some(Instant::now())
        } else {
            None
        };

        let word_size = if self.core.is_64_bit() { 8 } else { 4 };
        let bytes = if bytes.len().is_multiple_of(word_size) {
            Cow::Borrowed(bytes)
        } else {
            let mut bytes = bytes.to_vec();
            // Pad the bytes to the next word size.
            bytes.resize(
                bytes.len().div_ceil(word_size) * word_size,
                self.flash_algorithm.flash_properties.erased_byte_value,
            );
            Cow::Owned(bytes)
        };

        self.core.write(address, &bytes).map_err(FlashError::Core)?;

        if let Some(t1) = t1 {
            tracing::info!(
                "Took {:?} to download {} byte page into ram",
                t1.elapsed(),
                bytes.len()
            );
        };

        Ok(())
    }

    /// Runs the [`BlankCheck`] function.
    ///
    /// [`BlankCheck`]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/algorithmFunc.html#BlankCheck
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

impl ActiveFlasher<'_, '_, Erase> {
    /// Runs the [`EraseChip`] function.
    ///
    /// [`EraseChip`]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/algorithmFunc.html#EraseChip
    pub fn erase_all(&mut self) -> Result<(), FlashError> {
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

    /// Runs the [`EraseSector`] function.
    ///
    /// [`EraseSector`]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/algorithmFunc.html#EraseSector
    pub fn erase_sector(&mut self, sector: &FlashSector) -> Result<(), FlashError> {
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
}

impl ActiveFlasher<'_, '_, Program> {
    /// Runs the [`ProgramPage`] function.
    ///
    /// [`ProgramPage`]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/algorithmFunc.html#ProgramPage
    pub fn program_page(&mut self, page: &FlashPage) -> Result<(), FlashError> {
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

    /// Starts executing the [`ProgramPage`] function.
    ///
    /// This function can be used along with [`wait_for_write_end`][Self::wait_for_write_end] to implement double buffered programming.
    ///
    /// [`ProgramPage`]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/algorithmFunc.html#ProgramPage
    pub fn start_program_page_with_buffer(
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

    /// Waits for the write operation to complete.
    ///
    /// This function can be used along with [`start_program_page_with_buffer`][Self::start_program_page_with_buffer] to implement double buffered programming.
    ///
    /// [`start_program_page_with_buffer`]: Self::start_program_page_with_buffer
    pub fn wait_for_write_end(&mut self, last_page_address: u64) -> Result<(), FlashError> {
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
