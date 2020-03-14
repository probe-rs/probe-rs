use super::FlashProgress;
use super::{FlashBuilder, FlashError, FlashPage, FlashSector};
use crate::config::{FlashAlgorithm, FlashRegion, MemoryRange, SectorInfo};
use crate::core::{Core, RegisterFile};
use crate::error;
use crate::memory::MemoryInterface;
use crate::{session::Session, DebugProbeError};
use std::time::{Duration, Instant};

pub(super) trait Operation {
    fn operation() -> u32;
    fn operation_name(&self) -> &str {
        match Self::operation() {
            1 => "Erase",
            2 => "Program",
            3 => "Verify",
            _ => "Unknown Operation",
        }
    }
}

pub(super) struct Erase;

impl Operation for Erase {
    fn operation() -> u32 {
        1
    }
}

pub(super) struct Program;

impl Operation for Program {
    fn operation() -> u32 {
        2
    }
}

pub(super) struct Verify;

impl Operation for Verify {
    fn operation() -> u32 {
        3
    }
}

pub(super) struct Flasher<'a> {
    session: Session,
    flash_algorithm: &'a FlashAlgorithm,
    region: &'a FlashRegion,
    double_buffering_supported: bool,
}

impl<'a> Flasher<'a> {
    pub(super) fn new(
        session: Session,
        flash_algorithm: &'a FlashAlgorithm,
        region: &'a FlashRegion,
    ) -> Self {
        Self {
            session,
            flash_algorithm,
            region,
            double_buffering_supported: false,
        }
    }

    pub(super) fn region(&self) -> &FlashRegion {
        &self.region
    }

    /// Returns the necessary information about the sector which `address` resides in
    /// if the address is inside the flash region.
    pub(super) fn sector_info(&self, address: u32) -> Option<SectorInfo> {
        self.flash_algorithm.sector_info(address)
    }

    pub(super) fn flash_algorithm(&self) -> &FlashAlgorithm {
        &self.flash_algorithm
    }

    pub(super) fn double_buffering_supported(&self) -> bool {
        self.double_buffering_supported
    }

    pub(super) fn init<'b, 's: 'b, O: Operation>(
        &'s mut self,
        mut address: Option<u32>,
        clock: Option<u32>,
    ) -> Result<ActiveFlasher<'b, O>, FlashError> {
        log::debug!("Initializing the flash algorithm.");
        let flasher = self;
        let algo = flasher.flash_algorithm;

        use capstone::arch::*;
        let cs = capstone::Capstone::new()
            .arm()
            .mode(arm::ArchMode::Thumb)
            .endian(capstone::Endian::Little)
            .build()
            .unwrap();
        let i = algo
            .instructions
            .iter()
            .map(|i| {
                [
                    *i as u8,
                    (*i >> 8) as u8,
                    (*i >> 16) as u8,
                    (*i >> 24) as u8,
                ]
            })
            .collect::<Vec<[u8; 4]>>()
            .iter()
            .flatten()
            .copied()
            .collect::<Vec<u8>>();

        let instructions = cs
            .disasm_all(i.as_slice(), u64::from(algo.load_address))
            .unwrap();

        for instruction in instructions.iter() {
            log::trace!("{}", instruction);
        }

        if address.is_none() {
            address = Some(flasher.region.flash_info().rom_start);
        }

        // Attach to memory and core.
        let mut core = flasher
            .session
            .attach_to_core(0)
            .map_err(FlashError::Memory)?;

        // TODO: Halt & reset target.
        log::debug!("Halting core.");
        let cpu_info = core.halt();
        log::debug!("PC = 0x{:08x}", cpu_info.unwrap().pc);
        core.wait_for_core_halted().map_err(FlashError::Core)?;
        log::debug!("Reset and halt");
        core.reset_and_halt().map_err(FlashError::Core)?;

        // TODO: Possible special preparation of the target such as enabling faster clocks for the flash e.g.

        // Load flash algorithm code into target RAM.
        log::debug!(
            "Loading algorithm into RAM at address 0x{:08x}",
            algo.load_address
        );

        core.write_block32(algo.load_address, algo.instructions.as_slice())
            .map_err(FlashError::Memory)?;

        let mut data = vec![0; algo.instructions.len()];
        core.read_block32(algo.load_address, &mut data)
            .map_err(FlashError::Memory)?;

        for (offset, (original, read_back)) in algo.instructions.iter().zip(data.iter()).enumerate()
        {
            if original != read_back {
                log::error!(
                    "Failed to verify flash algorithm. Data mismatch at address {:#08x}",
                    algo.load_address + (4 * offset) as u32
                );
                log::error!("Original instruction: {:#08x}", original);
                log::error!("Readback instruction: {:#08x}", read_back);

                log::error!("Original: {:x?}", &algo.instructions);
                log::error!("Readback: {:x?}", &data);

                return Err(FlashError::FlashAlgorithmNotLoaded);
            }
        }

        log::debug!("RAM contents match flashing algo blob.");

        log::debug!("Preparing Flasher for region:");
        log::debug!("{:#?}", &flasher.region);
        log::debug!(
            "Double buffering enabled: {}",
            flasher.double_buffering_supported
        );
        let mut flasher = ActiveFlasher {
            core,
            flash_algorithm: flasher.flash_algorithm,
            region: flasher.region,
            _double_buffering_supported: flasher.double_buffering_supported,
            _operation: core::marker::PhantomData,
        };

        flasher.init(address, clock)?;

        Ok(flasher)
    }

    pub(super) fn run_erase<T, E: From<FlashError>>(
        &mut self,
        f: impl FnOnce(&mut ActiveFlasher<Erase>) -> Result<T, E> + Sized,
    ) -> Result<T, E> {
        // TODO: Fix those values (None, None).
        let mut active = self.init(None, None)?;
        let r = f(&mut active)?;
        active.uninit()?;
        Ok(r)
    }

    pub(super) fn run_program<T, E: From<FlashError>>(
        &mut self,
        f: impl FnOnce(&mut ActiveFlasher<Program>) -> Result<T, E> + Sized,
    ) -> Result<T, E> {
        // TODO: Fix those values (None, None).
        let mut active = self.init(None, None)?;
        let r = f(&mut active)?;
        active.uninit()?;
        Ok(r)
    }

    pub(super) fn run_verify<T, E: From<FlashError>>(
        &mut self,
        f: impl FnOnce(&mut ActiveFlasher<Verify>) -> Result<T, E> + Sized,
    ) -> Result<T, E> {
        // TODO: Fix those values (None, None).
        let mut active = self.init(None, None)?;
        let r = f(&mut active)?;
        active.uninit()?;
        Ok(r)
    }

    /// Writes a single block of data to the flash.
    pub(super) fn flash_block(
        &mut self,
        address: u32,
        data: &[u8],
        progress: &FlashProgress,
        do_chip_erase: bool,
        _fast_verify: bool,
    ) -> Result<(), FlashError> {
        if !self
            .region
            .range
            .contains_range(&(address..address + data.len() as u32))
        {
            return Err(FlashError::AddressNotInRegion {
                address,
                region: self.region.clone(),
            });
        }

        let mut fb = FlashBuilder::new();
        fb.add_data(address, data).expect("Add Data failed");
        self.program(&mut fb, do_chip_erase, true, false, progress)
            .expect("Add Data failed");

        Ok(())
    }

    /// Program the contents of given `FlashBuilder` to the flash.
    ///
    /// If `restore_unwritten_bytes` is `true`, all bytes of a sector,
    /// that are not to be written during flashing will be read from the flash first
    /// and written again once the sector is erased.
    pub(super) fn program(
        &mut self,
        flash_builder: &FlashBuilder,
        mut do_chip_erase: bool,
        restore_unwritten_bytes: bool,
        enable_double_buffering: bool,
        progress: &FlashProgress,
    ) -> Result<(), FlashError> {
        // Convert the list of flash operations into flash sectors and pages.
        let sectors = flash_builder.build_sectors_and_pages(self, restore_unwritten_bytes)?;

        let num_pages = sectors.iter().map(|s| s.pages.len()).sum();
        let page_size = self.flash_algorithm().flash_properties.page_size;
        let sector_size: u32 = sectors.iter().map(|s| s.size).sum();

        progress.initialized(num_pages, sector_size as usize, page_size);

        // Check if there is even sectors to flash.
        if sectors.is_empty() || sectors[0].pages.is_empty() {
            // Nothing to do.
            return Ok(());
        }

        // If the flash algo doesn't support erase all, disable chip erase.
        if self.flash_algorithm().pc_erase_all.is_none() {
            do_chip_erase = false;
        }

        log::debug!("Full Chip Erase enabled: {:?}", do_chip_erase);
        log::debug!("Double Buffering enabled: {:?}", enable_double_buffering);

        // Erase all necessary sectors.
        progress.started_erasing();

        if do_chip_erase {
            self.chip_erase(&sectors, progress)?;
        } else {
            self.sector_erase(&sectors, progress)?;
        }

        // Flash all necessary pages.

        if self.double_buffering_supported() && enable_double_buffering {
            self.program_double_buffer(&sectors, progress)?;
        } else {
            self.program_simple(&sectors, progress)?;
        };

        Ok(())
    }

    /// Fills all the bytes of `current_page`.
    ///
    /// If `restore_unwritten_bytes` is `true`, all bytes of the page,
    /// that are not to be written during flashing will be read from the flash first
    /// and written again once the page is programmed.
    pub(super) fn fill_page(
        &mut self,
        current_page: &mut FlashPage,
        restore_unwritten_bytes: bool,
    ) -> Result<(), FlashError> {
        // The remaining bytes to be filled in at the end of the page.
        let remaining_bytes = current_page.size as usize - current_page.data.len();
        if current_page.data.len() != current_page.size as usize {
            let address_remaining_start = current_page.address + current_page.data.len() as u32;

            // Fill up the page with current page bytes until it's full.
            let old_data = if restore_unwritten_bytes {
                // Read all the remaining old bytes from flash to restore them later.
                let mut data = vec![0; remaining_bytes];
                self.run_verify(|active| {
                    active.read_block8(address_remaining_start, data.as_mut_slice())
                })?;
                data
            } else {
                // Set all the remaining bytes to their default erased value.
                vec![self.flash_algorithm().flash_properties.erased_byte_value; remaining_bytes]
            };
            current_page.data.extend(old_data);
        }
        Ok(())
    }

    /// Erase the entire flash of the chip.
    ///
    /// This takes the list of available sectors only for progress reporting reasons.
    /// It does not indeed erase single sectors but erases the entire flash.
    fn chip_erase(
        &mut self,
        sectors: &[FlashSector],
        progress: &FlashProgress,
    ) -> Result<(), FlashError> {
        progress.started_erasing();

        let mut t = std::time::Instant::now();
        let result = self.run_erase(|active| active.erase_all());
        for sector in sectors {
            progress.sector_erased(sector.page_size, t.elapsed().as_millis());
            t = std::time::Instant::now();
        }

        if result.is_ok() {
            progress.finished_erasing();
        } else {
            progress.failed_erasing();
        }
        result
    }

    /// Programs all sectors contained in `sectors`.
    fn program_simple(
        &mut self,
        sectors: &[FlashSector],
        progress: &FlashProgress,
    ) -> Result<(), FlashError> {
        progress.started_flashing();

        let mut t = std::time::Instant::now();
        let result = self.run_program(|active| {
            for page in FlashBuilder::pages(sectors) {
                active.program_page(page.address, page.data.as_slice())?;
                progress.page_programmed(page.size, t.elapsed().as_millis());
                t = std::time::Instant::now();
            }
            Ok(())
        });

        if result.is_ok() {
            progress.finished_programming();
        } else {
            progress.failed_programming();
        }

        result
    }

    /// Perform an erase of all sectors given in `sectors` which actually contain any pages.
    fn sector_erase(
        &mut self,
        sectors: &[FlashSector],
        progress: &FlashProgress,
    ) -> Result<(), FlashError> {
        progress.started_flashing();

        let mut t = std::time::Instant::now();
        let result = self.run_erase(|active| {
            for sector in sectors {
                if !sector.pages.is_empty() {
                    active.erase_sector(sector.address)?;
                    progress.sector_erased(sector.size, t.elapsed().as_millis());
                    t = std::time::Instant::now();
                }
            }
            Ok(())
        });

        if result.is_ok() {
            progress.finished_erasing();
        } else {
            progress.failed_erasing();
        }

        result
    }

    /// Flash a program using double buffering.
    ///
    /// UNTESTED
    fn program_double_buffer(
        &mut self,
        sectors: &[FlashSector],
        progress: &FlashProgress,
    ) -> Result<(), FlashError> {
        let mut current_buf = 0;

        progress.started_flashing();

        let mut t = std::time::Instant::now();
        let result = self.run_program(|active| {
            for page in FlashBuilder::pages(sectors) {
                // At the start of each loop cycle load the next page buffer into RAM.
                active.load_page_buffer(page.address, page.data.as_slice(), current_buf)?;

                // Then wait for the active RAM -> Flash copy process to finish.
                // Also check if it finished properly. If it didn't, return an error.
                let result = active.wait_for_completion(Duration::from_secs(2))?;
                progress.page_programmed(page.size, t.elapsed().as_millis());
                t = std::time::Instant::now();
                if result != 0 {
                    return Err(FlashError::PageWrite {
                        page_address: page.address,
                        error_code: result,
                    });
                }

                // Start the next copy process.
                active.start_program_page_with_buffer(page.address, current_buf)?;

                // Swap the buffers
                if current_buf == 1 {
                    current_buf = 0;
                } else {
                    current_buf = 1;
                }
            }

            Ok(())
        });

        if result.is_ok() {
            progress.finished_programming();
        } else {
            progress.failed_programming();
        }

        result
    }
}

pub(super) struct ActiveFlasher<'a, O: Operation> {
    core: Core,
    flash_algorithm: &'a FlashAlgorithm,
    region: &'a FlashRegion,
    _double_buffering_supported: bool,
    _operation: core::marker::PhantomData<O>,
}

impl<'a, O: Operation> ActiveFlasher<'a, O> {
    pub(super) fn init(
        &mut self,
        address: Option<u32>,
        clock: Option<u32>,
    ) -> Result<(), FlashError> {
        let algo = &self.flash_algorithm;
        log::debug!("Running init routine.");

        // Execute init routine if one is present.
        if let Some(pc_init) = algo.pc_init {
            let result = self.call_function_and_wait(
                pc_init,
                address,
                clock.or(Some(0)),
                Some(O::operation()),
                None,
                true,
            )?;

            if result != 0 {
                return Err(FlashError::RoutineCallFailed {
                    name: "init",
                    errorcode: result,
                });
            }
        }

        Ok(())
    }

    pub(super) fn region(&self) -> &FlashRegion {
        &self.region
    }

    pub(super) fn flash_algorithm(&self) -> &FlashAlgorithm {
        &&self.flash_algorithm
    }

    // pub(super) fn session_mut(&mut self) -> &mut Session {
    //     &mut self.session
    // }

    pub(super) fn uninit(&mut self) -> Result<(), FlashError> {
        log::debug!("Running uninit routine.");
        let algo = &self.flash_algorithm;

        if let Some(pc_uninit) = algo.pc_uninit {
            let result = self.call_function_and_wait(
                pc_uninit,
                Some(O::operation()),
                None,
                None,
                None,
                false,
            )?;

            if result != 0 {
                return Err(FlashError::RoutineCallFailed {
                    name: "uninit",
                    errorcode: result,
                });
            }
        }
        Ok(())
    }

    fn call_function_and_wait(
        &mut self,
        pc: u32,
        r0: Option<u32>,
        r1: Option<u32>,
        r2: Option<u32>,
        r3: Option<u32>,
        init: bool,
    ) -> Result<u32, FlashError> {
        self.call_function(pc, r0, r1, r2, r3, init)?;
        self.wait_for_completion(Duration::from_secs(2))
    }

    fn call_function(
        &mut self,
        pc: u32,
        r0: Option<u32>,
        r1: Option<u32>,
        r2: Option<u32>,
        r3: Option<u32>,
        init: bool,
    ) -> Result<(), FlashError> {
        log::debug!(
            "Calling routine {:08x}({:?}, {:?}, {:?}, {:?}, init={})",
            pc,
            r0,
            r1,
            r2,
            r3,
            init
        );

        let algo = &self.flash_algorithm;
        let regs: &'static RegisterFile = self.core.registers();

        [
            (regs.program_counter(), Some(pc)),
            (regs.argument_register(0), r0),
            (regs.argument_register(1), r1),
            (regs.argument_register(2), r2),
            (regs.argument_register(3), r3),
            (
                regs.platform_register(9),
                if init { Some(algo.static_base) } else { None },
            ),
            (
                regs.stack_pointer(),
                if init { Some(algo.begin_stack) } else { None },
            ),
            (regs.return_address(), Some(algo.load_address + 1)),
        ]
        .iter()
        .map(|(description, value)| {
            if let Some(v) = value {
                self.core.write_core_reg(description.address, *v)?;
                log::debug!(
                    "content of {:#x}: 0x{:08x} should be: 0x{:08x}",
                    description.address.0,
                    self.core.read_core_reg(description.address)?,
                    *v
                );
                Ok(())
            } else {
                Ok(())
            }
        })
        .collect::<Result<Vec<()>, error::Error>>()
        .map_err(FlashError::Core)?;

        // Resume target operation.
        self.core.run().map_err(FlashError::Core)?;

        Ok(())
    }

    pub(super) fn wait_for_completion(&mut self, timeout: Duration) -> Result<u32, FlashError> {
        log::debug!("Waiting for routine call completion.");
        let regs = self.core.registers();

        let start = Instant::now();

        loop {
            match self.core.wait_for_core_halted() {
                Ok(()) => break,
                Err(e) => {
                    log::warn!("Error while waiting for core halted: {}", e);
                }
            }

            if start.elapsed() > timeout {
                return Err(FlashError::Core(
                    crate::Error::Probe(DebugProbeError::Timeout).into(),
                ));
            }
        }

        let r = self
            .core
            .read_core_reg(regs.result_register(0).address)
            .map_err(FlashError::Core)?;
        Ok(r)
    }

    pub(super) fn read_block32(
        &mut self,
        address: u32,
        data: &mut [u32],
    ) -> Result<(), FlashError> {
        self.core
            .memory()
            .read_block32(address, data)
            .map_err(FlashError::Memory)?;
        Ok(())
    }

    pub(super) fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<(), FlashError> {
        self.core
            .memory()
            .read_block8(address, data)
            .map_err(FlashError::Memory)?;
        Ok(())
    }
}

impl<'a> ActiveFlasher<'a, Erase> {
    pub(super) fn erase_all(&mut self) -> Result<(), FlashError> {
        log::debug!("Erasing entire chip.");
        let flasher = self;
        let algo = flasher.flash_algorithm;

        if let Some(pc_erase_all) = algo.pc_erase_all {
            let result =
                flasher.call_function_and_wait(pc_erase_all, None, None, None, None, false)?;

            if result != 0 {
                Err(FlashError::RoutineCallFailed {
                    name: "erase_all",
                    errorcode: result,
                })
            } else {
                Ok(())
            }
        } else {
            Err(FlashError::RoutineNotSupported("erase_all"))
        }
    }

    pub(super) fn erase_sector(&mut self, address: u32) -> Result<(), FlashError> {
        log::info!("Erasing sector at address 0x{:08x}", address);
        let t1 = std::time::Instant::now();
        let flasher = self;
        let algo = flasher.flash_algorithm;

        let result = flasher.call_function_and_wait(
            algo.pc_erase_sector,
            Some(address),
            None,
            None,
            None,
            false,
        )?;
        log::info!(
            "Done erasing sector. Result is {}. This took {:?}",
            result,
            t1.elapsed()
        );

        if result != 0 {
            Err(FlashError::RoutineCallFailed {
                name: "erase_sector",
                errorcode: result,
            })
        } else {
            Ok(())
        }
    }
}

impl<'a> ActiveFlasher<'a, Program> {
    pub(super) fn program_page(&mut self, address: u32, bytes: &[u8]) -> Result<(), FlashError> {
        let t1 = std::time::Instant::now();
        let flasher = self;
        let algo = flasher.flash_algorithm;

        log::info!(
            "Flashing page at address {:#08x} with size: {}",
            address,
            bytes.len()
        );

        // Transfer the bytes to RAM.
        flasher
            .core
            .memory()
            .write_block8(algo.begin_data, bytes)
            .map_err(FlashError::Memory)?;
        let result = flasher.call_function_and_wait(
            algo.pc_program_page,
            Some(address),
            Some(bytes.len() as u32),
            Some(algo.begin_data),
            None,
            false,
        )?;
        log::info!("Flashing took: {:?}", t1.elapsed());

        if result != 0 {
            Err(FlashError::RoutineCallFailed {
                name: "program_page",
                errorcode: result,
            })
        } else {
            Ok(())
        }
    }

    pub(super) fn start_program_page_with_buffer(
        &mut self,
        address: u32,
        buffer_number: usize,
    ) -> Result<(), FlashError> {
        let flasher = self;
        let algo = flasher.flash_algorithm;

        // Check the buffer number.
        if buffer_number < algo.page_buffers.len() {
            return Err(FlashError::InvalidBufferNumber {
                n: buffer_number,
                max: algo.page_buffers.len(),
            });
        }

        flasher.call_function(
            algo.pc_program_page,
            Some(address),
            Some(flasher.flash_algorithm().flash_properties.page_size),
            Some(algo.page_buffers[buffer_number as usize]),
            None,
            false,
        )?;

        Ok(())
    }

    pub(super) fn load_page_buffer(
        &mut self,
        _address: u32,
        bytes: &[u8],
        buffer_number: usize,
    ) -> Result<(), FlashError> {
        let flasher = self;
        let algo = flasher.flash_algorithm;

        // Check the buffer number.
        if buffer_number < algo.page_buffers.len() {
            return Err(FlashError::InvalidBufferNumber {
                n: buffer_number,
                max: algo.page_buffers.len(),
            });
        }

        // TODO: Prevent security settings from locking the device.

        // Transfer the buffer bytes to RAM.
        flasher
            .core
            .memory()
            .write_block8(algo.page_buffers[buffer_number as usize], bytes)
            .map_err(FlashError::Memory)?;

        Ok(())
    }
}
