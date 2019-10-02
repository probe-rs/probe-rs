use probe::debug_probe::DebugProbeError;
use probe_rs_debug::session::Session;
use ihex::reader::{Reader, ReaderError};
use ihex::record::Record::*;

use coresight::access_ports::AccessPortError;
use log::{info, debug};

use console::Term;

use std::error::Error;
use std::fmt;
use std::thread;
use std::time;
use std::time::Instant;
use std::path::Path;
use std::io::Write;

#[derive(Debug)]
pub enum FlashError {
    ReaderError(ReaderError),
    AccessPortError(AccessPortError),
    DebugProbeError(DebugProbeError),
    IoError(std::io::Error),
}

impl fmt::Display for FlashError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use FlashError::*;

        match self {
            ReaderError(ref e) => e.fmt(f),
            AccessPortError(ref e) => e.fmt(f),
            DebugProbeError(ref e) => e.fmt(f),
            IoError(ref e) => e.fmt(f),
        }
    }
}

impl Error for FlashError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        use FlashError::*;

        match self {
            ReaderError(ref e) => Some(e),
            AccessPortError(ref e) => Some(e),
            DebugProbeError(ref e) => Some(e),
            IoError(ref e) => Some(e),
        }
    }
}

impl From<AccessPortError> for FlashError {
    fn from(e: AccessPortError) -> Self {
        FlashError::AccessPortError(e)
    }
}

impl From<DebugProbeError> for FlashError {
    fn from(e: DebugProbeError) -> Self {
        FlashError::DebugProbeError(e)
    }
}

impl From<ReaderError> for FlashError {
    fn from(e: ReaderError) -> Self {
        FlashError::ReaderError(e)
    }
}

impl From<std::io::Error> for FlashError {
    fn from(e: std::io::Error) -> Self {
        FlashError::IoError(e)
    }
}

pub fn download_hex<S: AsRef<Path>>(
    file_path: S,
    session: &mut Session,
    page_size: u32,
) -> Result<(), FlashError> {
    let mut extended_linear_address = 0;

    let mut total_bytes = 0;

    let mut output = Term::stdout();

    // Start timer.
    let instant = Instant::now();

    session.target.halt(&mut session.probe).unwrap();

    let hex_file = std::fs::read_to_string(file_path)?;
    let hex = Reader::new(&hex_file);

    if output.is_term() {
        write!(output,
            "Wrote {} total 32bit words in {:.2?} seconds. Current addr: {}",
            total_bytes, 0, 0,
        )?;
    }



    let mut last_erased_page = 0;
    erase_page(&mut session.probe, 0)?;

    for record in hex {
        let record = record?;
        match record {
            Data { offset, value } => {
                let offset = extended_linear_address | u32::from(offset);

                let mut last_erased_address =  (last_erased_page + 1) * page_size - 1;

                while last_erased_address < offset && last_erased_page < 255 {
                    erase_page(&mut session.probe, (last_erased_page + 1) * page_size)?;

                    last_erased_page += 1;
                    last_erased_address =  (last_erased_page + 1) * page_size - 1;
                }

                write_bytes(&mut session.probe, offset, value.as_slice())?;
                total_bytes += value.len();
                // Stop timer.
                let elapsed = instant.elapsed();

                if output.is_term() {
                    output.clear_line()?;
                    write!(output,
                        "Wrote {} total 32bit words in {:.2?} seconds. Current addr: {:#08x}",
                        total_bytes, elapsed, offset
                    )?;
                } else {
                    writeln!(output,
                        "Wrote {} total 32bit words in {:.2?} seconds. Current addr: {:#08x}",
                        total_bytes, elapsed, offset
                    )?;
                }

            },
            EndOfFile => {
                info!("End of file, stopping");
            },
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

    output.write_line("")?;

    session.target.reset(&mut session.probe)?;

    session.target.run(&mut session.probe)?;

    Ok(())
}

#[allow(non_snake_case)]
fn write_bytes<P: super::MI>(
    probe: &mut P,
    address: u32,
    data: &[u8],
) -> Result<(), AccessPortError> {
    let NVMC = 0x4001_E000;
    let NVMC_CONFIG = NVMC + 0x504;
    let WEN: u32 = 0x1;

    info!("Writing to address 0x{:08x}, len={}", address, data.len());

    debug!("Setting WEN bit in NVM");
    probe.write32(NVMC_CONFIG, WEN)?;

    probe.write_block8(
        address,
        data
    )
}

#[allow(non_snake_case)]
fn erase_page<P: super::MI>(probe: &mut P, address: u32) -> Result<(), AccessPortError> {
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

    info!("Finished erasing page");

    Ok(())
}
