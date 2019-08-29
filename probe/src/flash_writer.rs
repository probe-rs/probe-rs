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
    pub min_program_length: u32,
    /// Whether the CRC32-based analyzer is supported.
    pub analyzer_supported: bool,
    /// RAM base address where the analyzer code will be placed. There must be at
    /// least 0x600 free bytes after this address.
    pub analyzer_address: u32,
}

pub struct Flasher<'a> {
    session: &'a Session,
}



impl<'a> Flasher<'a> {
    pub fn init(&self) {
        let algo = self.session.target.get_flash_algorithm();
        let regs = self.session.target.get_basic_register_addresses();

        self.session.probe.write_block32(algo.load_address, algo.instructions);

        if let Some(pc_init) = algo.pc_init {
            self.call_function_and_wait(pc_init, Some(address), Some(clock), Some(operation.value), None, true);
        }
    }

    fn call_function_and_wait(&mut self, pc: u32, r0: Option<u32>, r1: Option<u32>, r2: Option<u32>, r3: Option<u32>, init: bool) -> u32 {
        self.call_function(pc, r0, r1, r2, r3, init);
        self.wait_for_completion()
    }

    fn call_function(&self, pc: u32, r0: Option<u32>, r1: Option<u32>, r2: Option<u32>, r3: Option<u32>, init: bool) {
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