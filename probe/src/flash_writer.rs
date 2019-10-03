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

pub fn download_hex<P: arm_memory::MI, S: Into<String>>(file_path: S, probe: &mut P, page_size: u32) -> Result<(), FlashError> {
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
fn write_bytes<P: arm_memory::MI>( probe: &mut P, address: u32, data: &[u8]) -> Result<(), AccessPortError> {
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
fn erase_page<P: arm_memory::MI>(probe: &mut P, address: u32) -> Result<(), AccessPortError> {
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