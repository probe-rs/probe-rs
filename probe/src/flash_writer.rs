use ihex::reader::{Reader, ReaderError};
use ihex::record::Record::*;

use coresight::access_ports::AccessPortError;
use log::info;
use scroll::Pread;
use std::error::Error;
use std::fmt;
use std::thread;
use std::time;
use std::time::Instant;
use crate::session::Session;
use memory::MI;
use crate::memory::MemoryRegion;

#[derive(Debug)]
pub enum FlashError {
    ReaderError(ReaderError),
    AccessPortError(AccessPortError),
}

impl fmt::Display for FlashError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use FlashError::*;

        match self {
            ReaderError(ref e) => e.fmt(f),
            AccessPortError(ref e) => e.fmt(f),
        }
    }
}

impl Error for FlashError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        use FlashError::*;

        match self {
            ReaderError(ref e) => Some(e),
            AccessPortError(ref e) => Some(e),
        }
    }
}

impl From<AccessPortError> for FlashError {
    fn from(e: AccessPortError) -> Self {
        FlashError::AccessPortError(e)
    }
}

impl From<ReaderError> for FlashError {
    fn from(e: ReaderError) -> Self {
        FlashError::ReaderError(e)
    }
}

pub fn download_hex<P: memory::MI, S: Into<String>>(file_path: S, probe: &mut P, page_size: u32) -> Result<(), FlashError> {
    let mut extended_linear_address = 0;

    let mut total_bytes = 0;

    // Start timer.
    let instant = Instant::now();

    let hex_file = std::fs::read_to_string(file_path.into()).unwrap();
    let hex = Reader::new(&hex_file);

    for record in hex {
        let record = record?;
        match record {
            Data { offset, value } => {
                let offset = extended_linear_address | u32::from(offset);

                if offset % page_size == 0 {
                    erase_page(probe, offset)?;
                }

                write_bytes(probe, offset, value.as_slice())?;
                total_bytes += value.len();
                // Stop timer.
                let elapsed = instant.elapsed();
                println!(
                    "Wrote {} total 32bit words in {:.2?} seconds. Current addr: {}",
                    total_bytes, elapsed, offset
                );
            }
            EndOfFile => return Ok(()),
            ExtendedSegmentAddress(_) => {
                unimplemented!();
            }
            StartSegmentAddress { .. } => (),
            ExtendedLinearAddress(address) => {
                extended_linear_address = u32::from(address) << 16;
            }
            StartLinearAddress(_) => (),
        };
    }

    Ok(())
}

#[allow(non_snake_case)]
fn write_bytes<P: memory::MI>( probe: &mut P, address: u32, data: &[u8]) -> Result<(), AccessPortError> {
    let NVMC = 0x4001_E000;
    let NVMC_CONFIG = NVMC + 0x504;
    let WEN: u32 = 0x1;

    info!("Writing to address 0x{:08x}", address);

    probe.write32(NVMC_CONFIG, WEN)?;
    probe.write_block8(
        address,
        data
    )
}

#[allow(non_snake_case)]
fn erase_page<P: memory::MI>(probe: &mut P, address: u32) -> Result<(), AccessPortError> {
    let NVMC = 0x4001_E000;
    let NVMC_READY = NVMC + 0x400;
    let NVMC_CONFIG = NVMC + 0x504;
    let NVMC_ERASEPAGE = NVMC + 0x508;
    let EEN: u32 = 0x2;

    info!("Erasing page {:04} (0x{:08x})", address / 1024, address);

    probe.write32(NVMC_CONFIG, EEN)?;
    probe.write32(NVMC_ERASEPAGE, address)?;

    let mut read_flag: u32 = probe.read32(NVMC_READY)?;

    while read_flag == 0 {
        info!("NVM busy (flag=0x{:08x}), waiting...", read_flag);
        read_flag = probe.read32(NVMC_READY)?;
        thread::sleep(time::Duration::from_millis(1));
    }

    Ok(())
}

#[derive(Debug, Default)]
pub struct FlashAlgorithm {
    /// Memory address where the flash algo instructions will be loaded to.
    pub load_address: u32,
    /// List of 32-bit words containing the position-independant code for the algo.
    pub instructions: &'static [u32],
    /// Address of the `Init()` entry point. Optional.
    pub pc_init: Option<u32>,
    /// Address of the `UnInit()` entry point. Optional.
    pub pc_uninit: Option<u32>,
    /// Address of the `ProgramPage()` entry point.
    pub pc_program_page: u32,
    /// Address of the `EraseSector()` entry point.
    pub pc_erase_sector: u32,
    /// Address of the `EraseAll()` entry point. Optional.
    pub pc_erase_all: Option<u32>,
    /// Initial value of the R9 register for calling flash algo entry points, which
    /// determines where the position-independant data resides.
    pub static_base: u32,
    /// Initial value of the stack pointer when calling any flash algo API.
    pub begin_stack: u32,
    /// Base address of the page buffer. Used if `page_buffers` is not provided.
    pub begin_data: u32,
    /// An optional list of base addresses for page buffers. The buffers must be at
    /// least as large as the region's page_size attribute. If at least 2 buffers are included in
    /// the list, then double buffered programming will be enabled.
    pub page_buffers: &'static [u32],
    pub min_program_length: Option<u32>,
    /// Whether the CRC32-based analyzer is supported.
    pub analyzer_supported: bool,
    /// RAM base address where the analyzer code will be placed. There must be at
    /// least 0x600 free bytes after this address.
    pub analyzer_address: u32,
}

pub trait Operation {
    fn operation() -> u32;
}

pub struct Erase;

impl Operation for Erase {
    fn operation() -> u32 { 1 }
}

pub struct Program;

impl Operation for Program {
    fn operation() -> u32 { 2 }
}

pub struct Verify;

impl Operation for Verify {
    fn operation() -> u32 { 3 }
}

pub enum FlasherError {
    Init(u32),
    Uninit(u32),
    EraseAll(u32),
    EraseAllNotSupported,
    EraseSector(u32, u32),
    ProgramPage(u32, u32),
    InvalidBufferNumber(u32, u32),
    UnalignedFlashWriteAddress,
    UnalignedPhraseLength,
    ProgramPhrase(u32, u32),
}

pub struct InactiveFlasher<'a> {
    session: &'a mut Session,
}

impl<'a> InactiveFlasher<'a> {
    pub fn init<O: Operation>(&mut self, address: Option<u32>, clock: Option<u32>) -> Result<ActiveFlasher<O>, FlasherError> {
        let algo = self.session.target.get_flash_algorithm();
        let regs = self.session.target.get_basic_register_addresses();

        // TODO: Halt & reset target.

        // TODO: Possible special preparation of the target such as enabling faster clocks for the flash e.g.

        // Load flash algorithm code into target RAM.
        self.session.probe.write_block32(algo.load_address, algo.instructions);

        let mut flasher = ActiveFlasher {
            session: self.session,
            region: MemoryRegion { page_size: 0 },
            _operation: core::marker::PhantomData,
        };

        // Execute init routine if one is present.
        if let Some(pc_init) = algo.pc_init {
            let result = flasher.call_function_and_wait(
                pc_init,
                address,
                clock,
                Some(O::operation()),
                None,
                true
            );

            if result != 0 {
                return Err(FlasherError::Init(result));
            }
        }

        Ok(flasher)
    }
}

pub struct ActiveFlasher<'a, O: Operation> {
    session: &'a mut Session,
    region: MemoryRegion,
    _operation: core::marker::PhantomData<O>,
}

impl<'a, O: Operation> ActiveFlasher<'a, O> {
    pub fn uninit(&mut self) -> Result<InactiveFlasher, FlasherError> {
        let algo = self.session.target.get_flash_algorithm();

        if let Some(pc_uninit) = algo.pc_uninit {
            let result = self.call_function_and_wait(
                pc_uninit,
                Some(O::operation()),
                None,
                None,
                None,
                false
            );

            if result != 0 {
                return Err(FlasherError::Uninit(result));
            }
        }

        Ok(InactiveFlasher {
            session: self.session,
        })
    }

    fn call_function_and_wait(&mut self, pc: u32, r0: Option<u32>, r1: Option<u32>, r2: Option<u32>, r3: Option<u32>, init: bool) -> u32 {
        self.call_function(pc, r0, r1, r2, r3, init);
        self.wait_for_completion()
    }

    fn call_function(&mut self, pc: u32, r0: Option<u32>, r1: Option<u32>, r2: Option<u32>, r3: Option<u32>, init: bool) {
        let algo = self.session.target.get_flash_algorithm();
        let regs = self.session.target.get_basic_register_addresses();
        [
            (regs.PC, Some(pc)),
            (regs.R0, r0),
            (regs.R1, r1),
            (regs.R2, r2),
            (regs.R3, r3),
            (regs.R9, if init { Some(algo.static_base) } else { None }),
            (regs.SP, if init { Some(algo.begin_stack) } else { None }),
            (regs.LR, Some(algo.load_address + 1)),
        ].into_iter().for_each(|(addr, value)| if let Some(v) = value {
            self.session.target.write_core_reg(&mut self.session.probe, *addr, *v);
        });

        // resume target
        self.session.target.run(&mut self.session.probe);
    }

    fn wait_for_completion(&mut self) -> u32 {
        let regs = self.session.target.get_basic_register_addresses();

        while self.session.target.wait_for_core_halted(&mut self.session.probe).is_err() {}

        self.session.target.read_core_reg(&mut self.session.probe, regs.R0).unwrap()
    }
}

impl <'a> ActiveFlasher<'a, Erase> {
    pub fn erase_all(&mut self) -> Result<(), FlasherError> {
        let algo = self.session.target.get_flash_algorithm();

        if let Some(pc_erase_all) = algo.pc_erase_all {
            let result = self.call_function_and_wait(
                pc_erase_all,
                None,
                None,
                None,
                None,
                false
            );

            if result != 0 {
                Err(FlasherError::EraseAll(result))
            } else {
                Ok(())
            }
        } else {
            Err(FlasherError::EraseAllNotSupported)
        }
    }

    pub fn erase_sector(&mut self, address: u32) -> Result<(), FlasherError> {
        let algo = self.session.target.get_flash_algorithm();

        let result = self.call_function_and_wait(
            algo.pc_erase_sector,
            Some(address),
            None,
            None,
            None,
            false
        );

        if result != 0 {
            Err(FlasherError::EraseSector(result, address))
        } else {
            Ok(())
        }
    }
}

impl <'a> ActiveFlasher<'a, Program> {
    pub fn program_page(&mut self, address: u32, bytes: &[u8]) -> Result<(), FlasherError> {
        let algo = self.session.target.get_flash_algorithm();

        // TODO: Prevent security settings from locking the device.

        // Transfer the bytes to RAM.
        self.session.probe.write_block8(algo.begin_data, bytes);

        let result = self.call_function_and_wait(
            algo.pc_program_page,
            Some(address),
            Some(bytes.len() as u32),
            Some(algo.begin_data),
            None,
            false
        );

        if result != 0 {
            Err(FlasherError::ProgramPage(result, address))
        } else {
            Ok(())
        }
    }

    pub fn start_program_page_with_buffer(&mut self, address: u32, buffer_number: u32) -> Result<(), FlasherError> {
        let algo = self.session.target.get_flash_algorithm();

        // Check the buffer number.
        if buffer_number < algo.page_buffers.len() as u32 {
            return Err(FlasherError::InvalidBufferNumber(buffer_number, algo.page_buffers.len() as u32));
        }

        self.call_function(
            algo.pc_program_page,
            Some(address),
            Some(self.region.page_size),
            Some(algo.page_buffers[buffer_number as usize]),
            None,
            false
        );

        Ok(())
    }

    pub fn load_page_buffer(&mut self, address: u32, bytes: &[u8], buffer_number: u32) -> Result<(), FlasherError> {
        let algo = self.session.target.get_flash_algorithm();

        // Check the buffer number.
        if buffer_number < algo.page_buffers.len() as u32 {
            return Err(FlasherError::InvalidBufferNumber(buffer_number, algo.page_buffers.len() as u32));
        }

        // TODO: Prevent security settings from locking the device.

        // Transfer the buffer bytes to RAM.
        self.session.probe.write_block8(algo.page_buffers[buffer_number as usize], bytes);

        Ok(())
    }

    pub fn program_phrase(&mut self, address: u32, bytes: &[u8]) -> Result<(), FlasherError> {
        let algo = self.session.target.get_flash_algorithm();

        // Get the minimum programming length. If none was specified, use the page size.
        let min_len = if let Some(min_program_length) = algo.min_program_length {
            min_program_length
        } else {
            self.region.page_size
        };

        // Require write address and length to be aligned to the minimum write size.
        if address % min_len != 0 {
            return Err(FlasherError::UnalignedFlashWriteAddress);
        }
        if bytes.len() as u32 % min_len != 0 {
            return Err(FlasherError::UnalignedPhraseLength);
        }

        // TODO: Prevent security settings from locking the device.

        // Transfer the phrase bytes to RAM.
        self.session.probe.write_block8(algo.begin_data, bytes);

        let result = self.call_function_and_wait(
            algo.pc_program_page,
            Some(address),
            Some(bytes.len() as u32),
            Some(algo.begin_data),
            None,
            false
        );

        if result != 0 {
            Err(FlasherError::ProgramPhrase(result, address))
        } else {
            Ok(())
        }
    }
}